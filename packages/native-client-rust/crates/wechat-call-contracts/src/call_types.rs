//! Call control contracts. These types are local API messages, not remote wire schemas.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const CALL_PROTOCOL_VERSION: u32 = 3;

/// Fixed track ID for outgoing call audio.
pub const AUDIO_SEND_TRACK_ID: u32 = 1;
/// Base track ID for incoming call audio (`2` in direct calls; `2 + member_id` in conferences).
pub const AUDIO_RECEIVE_TRACK_BASE: u32 = 2;
/// Fixed track ID for outgoing call video.
pub const VIDEO_SEND_TRACK_ID: u32 = 65_537;
/// Base track ID for incoming call video (`65538` in direct calls; `65538 + member_id` in conferences).
pub const VIDEO_RECEIVE_TRACK_BASE: u32 = 65_538;

/// The first message a client sends on the control socket. It announces which
/// protocol versions the client supports.
#[derive(Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallClientHello {
    /// Announce the protocol versions this client supports. Send it within five
    /// seconds of opening the control socket, before any command.
    #[serde(rename = "call.hello")]
    Hello {
        /// One to eight distinct positive version numbers. The daemon speaks a
        /// single version and answers `protocol.error` when the list does not
        /// include it.
        #[serde(rename = "supportedProtocolVersions")]
        supported_protocol_versions: Vec<u32>,
    },
}

/// Why the daemon refused the protocol versions a client announced.
#[derive(Clone, Copy, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CallProtocolErrorCode {
    /// The list does not include the version the daemon speaks, or no list was
    /// sent (a missing list means version 1).
    UnsupportedProtocolVersion,
    /// The list is empty, null, has duplicates or is otherwise malformed.
    InvalidProtocolVersion,
}

/// Calling operations that are available. A missing flag means unavailable.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallCapabilities {
    /// Starting a conference call in a group is available.
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_create: bool,
    /// Inviting more contacts into a connected conference call is available.
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_invite: bool,
    /// Joining a conference call from an invitation is available.
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_join: bool,
    /// Direct voice calls with one contact are available.
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub direct_audio: bool,
    /// Direct video calls are available.
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub direct_video: bool,
    /// Audio in conference calls is available.
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_audio: bool,
    /// Video in conference calls is available.
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_video: bool,
}
fn capability_unavailable(value: &bool) -> bool {
    !value
}

/// Video codec exchanged on a call media track.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallVideoCodec {
    /// Annex-B H.265 / HEVC frames.
    H265,
    /// Annex-B H.264 / AVC frames.
    H264,
    /// Standalone JPEG still frames.
    Jpeg,
}

/// Whether a video track carries WeChat's active wire codec directly or is transcoded by the daemon.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallVideoMode {
    /// Annex-B frames are passed straight through without re-encoding.
    Passthrough,
    /// The daemon transcodes between the client codec and WeChat's active wire codec.
    Transcoded,
}

/// Video codecs and bounds advertised by the daemon or a ready client.
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallVideoCapabilities {
    /// Video codecs accepted from a sending client, in preference order.
    #[serde(default)]
    pub send_codecs: Vec<CallVideoCodec>,
    /// Video codecs deliverable to a receiving client, in preference order.
    #[serde(default)]
    pub receive_codecs: Vec<CallVideoCodec>,
    /// Maximum frame width in pixels. Omitted when no cap is specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_width: Option<u16>,
    /// Maximum frame height in pixels. Omitted when no cap is specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_height: Option<u16>,
    /// Maximum frame rate in frames per second. Omitted when no cap is specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_fps: Option<u8>,
}

/// Negotiated format for one video track.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallVideoFormat {
    /// Codec carried on this track.
    pub codec: CallVideoCodec,
    /// Whether this track passes wire frames straight through or transcodes them.
    pub mode: CallVideoMode,
    /// Maximum frame width in pixels for this track.
    pub max_width: u16,
    /// Maximum frame height in pixels for this track.
    pub max_height: u16,
    /// Maximum frame rate in frames per second for this track.
    pub max_fps: u8,
    /// Target bitrate in kbps when encoding or transcoding. Omitted for pass-through or JPEG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bitrate_kbps: Option<u32>,
}

/// Which calls WebSocket a single-use browser ticket authorizes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallTicketSocket {
    /// The call control WebSocket (`/v1/accounts/{a}/calls/socket`).
    Control,
    /// The call media WebSocket (`/v1/accounts/{a}/calls/{call}/media`).
    Media,
}

/// Request body for issuing a short-lived single-use browser WebSocket ticket.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallTicketRequest {
    /// Which calls WebSocket this ticket authorizes.
    pub socket: CallTicketSocket,
    /// Required when `socket` is `media`; omitted when `socket` is `control`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub call_id: Option<String>,
}

/// Response returned when issuing a single-use browser WebSocket ticket.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CallTicketResponse {
    /// Opaque single-use ticket to pass as `?ticket=` on the matching WebSocket URL. Do not log it.
    pub ticket: String,
    /// Unix timestamp in seconds when the ticket expires.
    #[ts(type = "number")]
    pub expires_at: i64,
    /// Ticket lifetime in seconds from issuance.
    pub expires_in_seconds: u32,
    /// Which calls WebSocket this ticket authorizes.
    pub socket: CallTicketSocket,
    /// Bound call ID when `socket` is `media`. Omitted for `control`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub call_id: Option<String>,
}

/// The PCM format of call audio. The daemon currently supports only 16,000 Hz,
/// mono, 20 ms frames.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallAudioFormat {
    /// Samples per second, per channel.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u8,
    /// Length of one audio frame in milliseconds.
    pub frame_ms: u16,
}
impl Default for CallAudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            channels: 1,
            frame_ms: 20,
        }
    }
}

/// Who to call.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum CallTarget {
    /// A direct call with one contact.
    Direct {
        /// The contact to call. A group ID is not accepted.
        #[serde(rename = "contactId")]
        contact_id: String,
    },
    /// A new conference call in a group.
    Conference {
        /// The group to call, whose ID ends in `@chatroom`.
        #[serde(rename = "groupId")]
        group_id: String,
        /// Contacts to invite: between 1 and 100.
        invitees: Vec<String>,
    },
}

/// Whether a call is with one contact or with a group.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    /// A call with one contact.
    Direct,
    /// A call in a group.
    Conference,
}
/// Whether a call is voice or video, independent of the media currently
/// negotiated. A video call can continue with audio only.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallType {
    /// A voice call.
    Voice,
    /// A video call.
    Video,
}
/// Whether this account placed the call or received it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallDirection {
    /// Another party called this account.
    Incoming,
    /// This account placed the call.
    Outgoing,
}
/// Where a call is in its life. Stop sending audio at `ending`, and keep the
/// control socket connected while the daemon finishes cleanup.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    /// An incoming call is waiting to be answered or rejected.
    Offered,
    /// An outgoing call is being placed.
    Dialing,
    /// The other side is ringing.
    Ringing,
    /// The call was accepted and audio is being set up.
    Connecting,
    /// The call is up and audio can flow.
    Connected,
    /// The call is being hung up.
    Ending,
    /// The call is over.
    Ended,
    /// The outcome could not be confirmed.
    Uncertain,
}

/// An incoming call that can be answered or rejected now.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallOffer {
    /// ID of the offered call.
    pub call_id: String,
    /// Increases when the offer changes. Send the latest value as
    /// `expectedRevision` when answering or rejecting.
    pub revision: u32,
    /// Whether the call is direct or a conference.
    pub kind: CallKind,
    /// Whether the call is voice or video.
    pub call_type: CallType,
    /// Contact ID of the caller.
    pub caller_id: String,
}
/// How far along an incoming call is before it can be answered.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallPendingPhase {
    /// Waiting for a client to advertise readiness.
    WaitingForReady,
    /// The daemon is preparing the call.
    Preparing,
}
/// An incoming call that cannot be answered yet, because it is waiting for a
/// ready client or for the daemon to finish preparing it.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallPendingOffer {
    /// ID of the incoming call.
    pub call_id: String,
    /// Increases when the offer changes.
    pub revision: u32,
    /// Whether the call is direct or a conference.
    pub kind: CallKind,
    /// Whether the call is voice or video.
    pub call_type: CallType,
    /// Contact ID of the caller.
    pub caller_id: String,
    /// What the call is waiting for.
    pub phase: CallPendingPhase,
}
/// Which calls a readiness announcement covers.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum CallReadyScope {
    /// Incoming and outgoing calls.
    All,
    /// Outgoing calls only.
    Outgoing,
    /// One pending incoming call.
    Offer {
        /// ID of the pending incoming call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The offer revision you expect; a different revision is rejected.
        #[serde(rename = "expectedRevision")]
        expected_revision: u32,
    },
}
/// A call this account is in.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallView {
    /// ID of the call.
    pub call_id: String,
    /// Increases each time the call changes. Send the latest value as
    /// `expectedRevision`.
    pub revision: u32,
    /// Whether the call is direct or a conference.
    pub kind: CallKind,
    /// Whether the call is voice or video.
    pub call_type: CallType,
    /// Whether this account placed the call or received it.
    pub direction: CallDirection,
    /// Where the call is in its life.
    pub state: CallState,
    /// True when this connection owns the call. Always false in HTTP snapshots.
    pub owned_by_this_client: bool,
    /// The group of a conference call. Omitted for a direct call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub group_id: Option<String>,
    /// The latest conference roster. Omitted for a direct call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<_>")]
    pub participants: Vec<CallParticipant>,
    /// The latest controls reported by the other party of a direct call, also
    /// given to a newly connected observer. Omitted until one is reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<_>")]
    pub controls: Vec<CallParticipantControl>,
}

/// One participant in a conference call.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallParticipant {
    /// ID of the participant within this call. It is not a contact ID; use it
    /// to match audio tracks to participants.
    pub participant_id: String,
    /// Contact ID of the participant.
    pub contact_id: String,
    /// Whether the participant is in the room.
    pub membership: CallMembership,
    /// Signaling progress, separate from membership. Omitted until reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<CallParticipantStatus>,
    /// Whether the participant's microphone is on. Omitted until the server
    /// reports it; an omitted value is unknown, not false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub microphone_enabled: Option<bool>,
    /// Whether the participant's camera is on. Omitted until the server
    /// reports it; an omitted value is unknown, not false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub camera_enabled: Option<bool>,
}

/// Whether a participant is in a conference call.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallMembership {
    /// In the room.
    Joined,
    /// Invited but not in the room.
    Invited,
    /// Was in the room and left.
    Left,
    /// The server has not said.
    Unknown,
}

/// Signaling progress of a participant, independent of membership.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallParticipantStatus {
    /// The status number the service reported. Keep it for codes `kind` does
    /// not name.
    pub code: u32,
    /// What the code means.
    pub kind: CallParticipantStatusKind,
}

/// What a participant status code means. Cancelled, busy, rejected and no
/// response do not by themselves mean a participant left.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallParticipantStatusKind {
    /// Code 0: no state reported.
    Undefined,
    /// Code 1: in the room.
    Joined,
    /// Code 2: invited, not in the room.
    Invited,
    /// Code 3: invited and acknowledged, not in the room.
    Acknowledged,
    /// Code 4: left the room.
    Left,
    /// Code 5: cancelled.
    Cancelled,
    /// Code 6: busy.
    Busy,
    /// Code 7: rejected.
    Rejected,
    /// Code 8: no response.
    NoResponse,
    /// Any other code. No join or departure is implied.
    Unknown,
}

/// A conference call this account can join. It carries no room credentials.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallConferenceInvitation {
    /// Opaque ID for this account. Send it in `call.join`.
    pub invitation_id: String,
    /// Revision of the invitation. Send it in `call.join` exactly as listed.
    pub revision: u32,
    /// The group the conference call is in.
    pub group_id: String,
    /// Contact ID of the caller.
    pub caller_id: String,
}

/// A control a call participant can report. A microphone or camera update does
/// not reset the call's audio format.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum CallControl {
    /// The participant's microphone.
    Microphone {
        /// True when the microphone is muted.
        muted: bool,
    },
    /// The participant's camera.
    Camera {
        /// True when the camera is sending video.
        enabled: bool,
    },
}
/// A control reported by one participant.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallParticipantControl {
    /// The participant the control belongs to. In a direct call this is the
    /// contact ID.
    pub participant_id: String,
    /// The control and its value.
    pub control: CallControl,
}
/// Whether this account is in a call.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallsAvailability {
    /// True while this account has a call, including one whose outcome is
    /// unresolved, so a new call cannot start.
    pub busy: bool,
    /// True when this connection owns that call.
    pub owned_by_this_client: bool,
}
/// The state of calling for one account, as returned by the HTTP snapshot
/// route. The `hello` event carries the same fields.
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallSnapshot {
    /// Conference calls this account can join.
    #[serde(default)]
    pub conference_invitations: Vec<CallConferenceInvitation>,
    /// Calling operations that are available.
    pub capabilities: CallCapabilities,
    /// Whether this account is in a call.
    pub availability: CallsAvailability,
    /// The incoming call that can be answered now, or null.
    pub current_offer: Option<CallOffer>,
    /// An incoming call that cannot be answered yet, or null.
    #[serde(default)]
    pub current_pending_offer: Option<CallPendingOffer>,
    /// The active call, or null.
    pub current_call: Option<CallView>,
    /// The PCM format used for call audio.
    pub audio_format: CallAudioFormat,
    /// Video codecs and bounds this daemon can exchange. Omitted when video is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub video_capabilities: Option<CallVideoCapabilities>,
}

/// A command a client sends on the control socket. Every command has a
/// `requestId`; reuse it only to repeat an identical command.
#[derive(Clone, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallCommand {
    /// Invite more contacts into a connected conference call you own. Results
    /// arrive as `call.invitation_result`.
    #[serde(rename = "call.invite")]
    Invite {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// ID of the conference call.
        #[serde(rename = "callId")]
        call_id: String,
        /// Contacts to invite: between 1 and 100.
        #[serde(rename = "contactIds")]
        contact_ids: Vec<String>,
    },
    /// Join a conference call from one of the listed invitations. The socket
    /// becomes the call's owner, as with `call.dial`.
    #[serde(rename = "call.join")]
    Join {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// The `invitationId` of an entry in `conferenceInvitations`.
        #[serde(rename = "invitationId")]
        invitation_id: String,
        /// The `revision` of that entry. A stale or used invitation fails with
        /// `offer_expired`.
        #[serde(rename = "invitationRevision")]
        invitation_revision: u32,
        /// Whether to join with your camera enabled. Omitted means false.
        #[serde(rename = "cameraEnabled")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(rename = "cameraEnabled", optional)]
        camera_enabled: Option<bool>,
        /// Whether to join with your microphone muted. Omitted means false.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        muted: Option<bool>,
    },
    /// Advertise or withdraw readiness to handle call media. A new connection
    /// is passive until it advertises readiness.
    #[serde(rename = "call.ready")]
    Ready {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// True to advertise readiness, false to withdraw it.
        ready: bool,
        /// The PCM format you will send and receive. It must match the
        /// daemon's audio format.
        format: CallAudioFormat,
        /// Which calls readiness covers. Omitted means all. Ignored when
        /// withdrawing readiness.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        scope: Option<CallReadyScope>,
        /// Video codecs and bounds this client supports. Omitted for audio-only clients.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        video: Option<CallVideoCapabilities>,
    },
    /// Start an outgoing call. The socket becomes the call's owner.
    #[serde(rename = "call.dial")]
    Dial {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// Who to call.
        target: CallTarget,
        /// Whether to place a voice or video call.
        #[serde(rename = "callType")]
        call_type: CallType,
        /// Initial camera state for video calls. Defaults to true when `callType` is `video`, ignored for voice calls.
        #[serde(rename = "cameraEnabled")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(rename = "cameraEnabled", optional)]
        camera_enabled: Option<bool>,
        /// Initial microphone mute state. Omitted means false.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        muted: Option<bool>,
    },
    /// Answer the incoming call. The first client to answer owns the call;
    /// others get `already_claimed`.
    #[serde(rename = "call.answer")]
    Answer {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// ID of the offered call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The latest offer `revision` you received. A stale revision is
        /// rejected.
        #[serde(rename = "expectedRevision")]
        expected_revision: u32,
        /// Initial camera state when answering a video call. Defaults to true for video calls, ignored for voice calls.
        #[serde(rename = "cameraEnabled")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(rename = "cameraEnabled", optional)]
        camera_enabled: Option<bool>,
        /// Initial microphone mute state when answering. Omitted means false.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        muted: Option<bool>,
    },
    /// Reject the incoming call.
    #[serde(rename = "call.reject")]
    Reject {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// ID of the offered call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The latest offer `revision` you received. A stale revision is
        /// rejected.
        #[serde(rename = "expectedRevision")]
        expected_revision: u32,
    },
    /// Cancel your outgoing call before the other side picks up.
    #[serde(rename = "call.cancel")]
    Cancel {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// ID of your outgoing call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The latest call `revision` you received. A stale revision is
        /// rejected.
        #[serde(rename = "expectedRevision")]
        expected_revision: u32,
    },
    /// End a call after answering or connecting. In a conference this leaves
    /// the call without ending it for the others.
    #[serde(rename = "call.hangup")]
    Hangup {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// ID of the call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The latest call `revision` you received. A stale revision is
        /// rejected.
        #[serde(rename = "expectedRevision")]
        expected_revision: u32,
    },
    /// Mute or unmute your outgoing audio. This also clears queued audio and
    /// starts a new send generation, announced by `track.updated`.
    #[serde(rename = "call.mute")]
    Mute {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// ID of the call.
        #[serde(rename = "callId")]
        call_id: String,
        /// True to mute, false to unmute.
        muted: bool,
    },
    /// Discard queued outgoing audio, for example to interrupt a reply. This
    /// starts a new send generation, announced by `track.updated`.
    #[serde(rename = "call.clear_audio")]
    ClearAudio {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// ID of the call.
        #[serde(rename = "callId")]
        call_id: String,
    },
    /// Turn your outgoing camera on or off during a video or conference call.
    /// Starts a new video send generation, announced by `track.updated`.
    #[serde(rename = "call.camera")]
    Camera {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// ID of the call.
        #[serde(rename = "callId")]
        call_id: String,
        /// True to enable outgoing video, false to disable it.
        enabled: bool,
    },
}
impl CallCommand {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Ready { request_id, .. }
            | Self::Invite { request_id, .. }
            | Self::Join { request_id, .. }
            | Self::Dial { request_id, .. }
            | Self::Answer { request_id, .. }
            | Self::Reject { request_id, .. }
            | Self::Cancel { request_id, .. }
            | Self::Hangup { request_id, .. }
            | Self::Mute { request_id, .. }
            | Self::ClearAudio { request_id, .. }
            | Self::Camera { request_id, .. } => request_id,
        }
    }
}

/// How the daemon handled a command.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallCommandStatus {
    /// The command started. This does not mean the call connected.
    Accepted,
    /// An identical earlier command was already accepted.
    Replayed,
    /// The command was rejected; see `code`.
    Error,
}
/// Why a command or socket failed. Lost replies do not authorize an automatic
/// retry.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallErrorCode {
    /// The message was malformed or a value was out of range.
    InvalidRequest,
    /// This account is already in a call, or an earlier call's outcome is
    /// unresolved.
    Busy,
    /// Another client already answered the incoming call.
    AlreadyClaimed,
    /// The offer or invitation is no longer available.
    OfferExpired,
    /// This socket does not own the call.
    NotOwner,
    /// The daemon does not support this operation.
    Unsupported,
    /// No client has advertised readiness. Send `call.ready` first.
    MediaNotReady,
    /// An earlier call's outcome was never confirmed. Do not retry
    /// automatically.
    OutcomeUnknown,
    /// A limit was reached, such as too much queued audio.
    Capacity,
    /// The revision, generation or connection is out of date.
    Stale,
    /// The token's scopes do not allow this action.
    Forbidden,
    /// The connection or call is closed.
    Closed,
}

/// Whether a media track carries media you send or media you receive.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallTrackDirection {
    /// Media from you to the call.
    Send,
    /// Media from the call to you.
    Receive,
}
/// One audio or video stream: a track you send, or one source you receive.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(tag = "media", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "media", rename_all = "snake_case")]
pub enum CallTrack {
    /// One PCM audio stream.
    Audio {
        /// Track ID to use in `track.start`, `video.request_keyframe` and binary `WXCM` frames.
        id: u32,
        /// Version of the stream. Frames for an older generation are discarded; a
        /// new generation starts after `call.clear_audio`, a mute change, a camera
        /// toggle, or a format change.
        generation: u32,
        /// Whether you send or receive media on this track.
        direction: CallTrackDirection,
        /// The PCM format of the track.
        format: CallAudioFormat,
        /// The conference participant this track carries. Omitted when the track
        /// is not tied to a participant.
        #[serde(rename = "participantId")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(rename = "participantId", optional)]
        participant_id: Option<String>,
    },
    /// One video stream.
    Video {
        /// Track ID to use in `track.start`, `video.request_keyframe` and binary `WXCM` frames.
        id: u32,
        /// Version of the stream. Frames for an older generation are discarded; a
        /// new generation starts after `call.clear_audio`, a mute change, a camera
        /// toggle, or a format change.
        generation: u32,
        /// Whether you send or receive media on this track.
        direction: CallTrackDirection,
        /// The negotiated video codec, mode and bounds of the track.
        format: CallVideoFormat,
        /// The conference participant this track carries. Omitted when the track
        /// is not tied to a participant.
        #[serde(rename = "participantId")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(rename = "participantId", optional)]
        participant_id: Option<String>,
        /// True when this video track is currently active.
        #[serde(default)]
        active: bool,
    },
}

impl CallTrack {
    /// Return the track's numeric ID.
    pub fn id(&self) -> u32 {
        match self {
            Self::Audio { id, .. } | Self::Video { id, .. } => *id,
        }
    }
    /// Return the track's current generation.
    pub fn generation(&self) -> u32 {
        match self {
            Self::Audio { generation, .. } | Self::Video { generation, .. } => *generation,
        }
    }
    /// Return the track's direction.
    pub fn direction(&self) -> CallTrackDirection {
        match self {
            Self::Audio { direction, .. } | Self::Video { direction, .. } => *direction,
        }
    }
    /// Return true when this track carries video.
    pub fn is_video(&self) -> bool {
        matches!(self, Self::Video { .. })
    }
    /// Return the audio format if this is an audio track.
    pub fn audio_format(&self) -> Option<CallAudioFormat> {
        match self {
            Self::Audio { format, .. } => Some(*format),
            Self::Video { .. } => None,
        }
    }
    /// Return the video format if this is a video track.
    pub fn video_format(&self) -> Option<CallVideoFormat> {
        match self {
            Self::Video { format, .. } => Some(*format),
            Self::Audio { .. } => None,
        }
    }
    /// Return the participant ID associated with this track, if any.
    pub fn participant_id(&self) -> Option<&str> {
        match self {
            Self::Audio { participant_id, .. } | Self::Video { participant_id, .. } => {
                participant_id.as_deref()
            }
        }
    }
}

/// What happened to one invitation. It is separate from room membership.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallInvitationStatus {
    /// The service accepted the invitation.
    Submitted,
    /// The service rejected the invitation.
    Rejected,
    /// The service gave no result for this contact.
    Unknown,
    /// The invitation may have reached the service. Do not retry
    /// automatically.
    Uncertain,
}

/// The outcome of inviting one contact. It does not mean the contact joined.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallInvitationResult {
    /// The contact this result is for.
    pub contact_id: String,
    /// What happened to the invitation.
    pub status: CallInvitationStatus,
    /// The code the service returned. Omitted when unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub service_code: Option<u32>,
    /// True when the service asked for a verification step. The event carries
    /// no verification link.
    pub verification_required: bool,
}

/// An event the daemon sends. Successful owner results include a secret
/// `ownerToken`, so do not log whole events.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallEvent {
    /// The outcome of each invited contact for a create or invite command, sent
    /// to the call's owner.
    #[serde(rename = "call.invitation_result")]
    InvitationResult {
        /// ID of the conference call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The `requestId` of the create or invite command.
        #[serde(rename = "requestId")]
        request_id: String,
        /// One result per invited contact.
        results: Vec<CallInvitationResult>,
    },
    /// The protocol version could not be agreed. The daemon then closes the
    /// socket with WebSocket code 1002.
    #[serde(rename = "protocol.error")]
    ProtocolError {
        /// Why the versions were refused.
        code: CallProtocolErrorCode,
        /// The versions the client announced. Empty when the list was invalid.
        #[serde(rename = "clientVersions")]
        client_versions: Vec<u32>,
        /// The versions the daemon supports.
        #[serde(rename = "serverVersions")]
        server_versions: Vec<u32>,
    },
    /// Replaces the list of conference calls this account can join. An empty
    /// list clears it.
    #[serde(rename = "conference.invitations")]
    ConferenceInvitations {
        /// The current invitations.
        invitations: Vec<CallConferenceInvitation>,
    },
    /// The first event on the control socket: this connection's ID and the
    /// current calling state. Check `protocolVersion` before sending commands.
    #[serde(rename = "hello")]
    Hello {
        /// Conference calls this account can join.
        #[serde(rename = "conferenceInvitations", default)]
        conference_invitations: Vec<CallConferenceInvitation>,
        /// The protocol version the daemon speaks on this socket.
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        /// ID of this connection. Send it as `clientId` in `media.attach`.
        #[serde(rename = "clientId")]
        client_id: String,
        /// Calling operations that are available.
        capabilities: CallCapabilities,
        /// Whether this account is in a call.
        availability: CallsAvailability,
        /// The incoming call that can be answered now, or null.
        #[serde(rename = "currentOffer")]
        current_offer: Option<CallOffer>,
        /// An incoming call that cannot be answered yet, or null.
        #[serde(rename = "currentPendingOffer", default)]
        current_pending_offer: Option<CallPendingOffer>,
        /// The active call, or null.
        #[serde(rename = "currentCall")]
        current_call: Option<CallView>,
        /// The PCM format used for call audio.
        #[serde(rename = "audioFormat")]
        audio_format: CallAudioFormat,
        /// Video codecs and bounds this daemon can exchange. Omitted when video is unavailable.
        #[serde(rename = "videoCapabilities")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(rename = "videoCapabilities", optional)]
        video_capabilities: Option<CallVideoCapabilities>,
    },
    /// The result of a command, matched to it by `requestId`. Acceptance starts
    /// the operation; it does not mean the call connected.
    #[serde(rename = "command.result")]
    CommandResult {
        /// The `requestId` of the command.
        #[serde(rename = "requestId")]
        request_id: String,
        /// Whether the command was accepted, repeated or rejected.
        status: CallCommandStatus,
        /// The call the command claimed. Null unless it was a dial, answer or
        /// join that succeeded.
        #[serde(rename = "callId")]
        call_id: Option<String>,
        /// Secret token for `media.attach` on the claimed call. Null unless a
        /// call was claimed. Do not log it.
        #[serde(rename = "ownerToken")]
        owner_token: Option<String>,
        /// Why the command failed. Null unless `status` is `error`.
        code: Option<CallErrorCode>,
    },
    /// An incoming call that can be answered or rejected now.
    #[serde(rename = "call.offered")]
    Offered {
        /// The offer. Send its `revision` in `call.answer` or `call.reject`.
        offer: CallOffer,
    },
    /// An incoming call that cannot be answered yet, or null when none is
    /// pending.
    #[serde(rename = "call.pending")]
    Pending {
        /// The pending call, or null to clear it.
        offer: Option<CallPendingOffer>,
    },
    /// The call changed. Sent to every client of the same account.
    #[serde(rename = "call.state")]
    State {
        /// The call as it is now.
        call: CallView,
    },
    /// A conference participant's membership or microphone changed. A
    /// `call.state` with the new roster follows.
    #[serde(rename = "participant.updated")]
    ParticipantUpdated {
        /// ID of the conference call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The call revision after this change.
        revision: u32,
        /// The participant as it is now.
        participant: CallParticipant,
    },
    /// The other party of a direct call changed a control, such as muting their
    /// microphone. It needs no reply.
    #[serde(rename = "call.control")]
    Control {
        /// ID of the direct call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The contact whose control changed.
        #[serde(rename = "participantId")]
        participant_id: String,
        /// The control and its new value.
        control: CallControl,
    },
    /// Whether this account is in a call changed.
    #[serde(rename = "calls.availability")]
    Availability {
        /// The new availability.
        availability: CallsAvailability,
    },
    /// A receive audio or video track appeared. Frames for it follow.
    #[serde(rename = "track.added")]
    TrackAdded {
        /// ID of the call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The new track.
        track: CallTrack,
    },
    /// A track moved to a new generation or state, for example after
    /// `call.clear_audio`, a mute change, or `call.camera`. Send new frames on
    /// the new generation.
    #[serde(rename = "track.updated")]
    TrackUpdated {
        /// ID of the call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The track with its new generation or state.
        track: CallTrack,
    },
    /// A receive track ended. Frames for that generation stop.
    #[serde(rename = "track.removed")]
    TrackRemoved {
        /// ID of the call.
        #[serde(rename = "callId")]
        call_id: String,
        /// ID of the track that ended.
        #[serde(rename = "trackId")]
        track_id: u32,
        /// The generation that ended.
        generation: u32,
        /// Why the track ended. Currently `source_unavailable`.
        reason: String,
    },
    /// Sent on the media socket after `media.attach` succeeds: the audio and
    /// video tracks to use.
    #[serde(rename = "media.ready")]
    MediaReady {
        /// The protocol version the daemon speaks on the media socket.
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        /// The tracks for media you send (audio, plus video when negotiated).
        #[serde(rename = "sendTracks")]
        send_tracks: Vec<CallTrack>,
        /// The tracks for media you receive, one per audio or video source.
        #[serde(rename = "receiveTracks")]
        receive_tracks: Vec<CallTrack>,
    },
    /// Sent on the media socket when the remote party asks your video encoder
    /// for a fresh keyframe (IDR with inline parameter sets).
    #[serde(rename = "video.keyframe_requested")]
    VideoKeyframeRequested {
        /// ID of the call.
        #[serde(rename = "callId")]
        call_id: String,
        /// ID of your outgoing video track.
        #[serde(rename = "trackId")]
        track_id: u32,
        /// Generation of your outgoing video track.
        generation: u32,
    },
    /// A failure that is not tied to a command, such as an invalid message or a
    /// missing permission. The daemon then closes the socket, except for
    /// `outcome_unknown`, which follows `hello` and leaves it open.
    #[serde(rename = "call.error")]
    Error {
        /// What went wrong.
        code: CallErrorCode,
    },
}

/// A JSON control message sent by the client on the media socket. Audio and
/// video payloads travel as binary WebSocket messages with a 36-byte `WXCM` v3
/// header.
#[derive(Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallMediaCommand {
    /// Attach the socket to a call you own. Send it first, within five seconds
    /// of connecting; the daemon answers `media.ready`.
    #[serde(rename = "media.attach")]
    Attach {
        /// The protocol versions you support.
        #[serde(rename = "supportedProtocolVersions")]
        supported_protocol_versions: Vec<u32>,
        /// The `clientId` from the control socket's `hello`.
        #[serde(rename = "clientId")]
        client_id: String,
        /// The secret `ownerToken` from the `command.result` that claimed the
        /// call. Do not log it.
        #[serde(rename = "ownerToken")]
        owner_token: String,
    },
    /// Start sending media on one of your send tracks for a generation. Send it
    /// before the first binary frame, and again after each new generation.
    #[serde(rename = "track.start")]
    TrackStart {
        /// The send track's ID.
        #[serde(rename = "trackId")]
        track_id: u32,
        /// The generation to start.
        generation: u32,
    },
    /// Ask the remote sender of a receive video track to emit a fresh keyframe.
    #[serde(rename = "video.request_keyframe")]
    VideoRequestKeyframe {
        /// The receive video track's ID.
        #[serde(rename = "trackId")]
        track_id: u32,
    },
}
