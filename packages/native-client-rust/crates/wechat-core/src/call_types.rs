//! Call control contracts. These types are local API messages, not remote wire schemas.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const CALL_PROTOCOL_VERSION: u32 = 2;

#[derive(Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallClientHello {
    #[serde(rename = "call.hello")]
    Hello {
        #[serde(rename = "supportedProtocolVersions")]
        supported_protocol_versions: Vec<u32>,
    },
}

#[derive(Clone, Copy, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CallProtocolErrorCode {
    UnsupportedProtocolVersion,
    InvalidProtocolVersion,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallCapabilities {
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_create: bool,
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_invite: bool,
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_join: bool,
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub direct_audio: bool,
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub direct_video: bool,
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_audio: bool,
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_video: bool,
}
fn capability_unavailable(value: &bool) -> bool {
    !value
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallAudioFormat {
    pub sample_rate: u32,
    pub channels: u8,
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

#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum CallTarget {
    Direct {
        #[serde(rename = "contactId")]
        contact_id: String,
    },
    Conference {
        #[serde(rename = "groupId")]
        group_id: String,
        invitees: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    Direct,
    Conference,
}
/// Call identity, independent from membership and the currently negotiated media.
/// A video call can continue with audio only.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallType {
    Voice,
    Video,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallDirection {
    Incoming,
    Outgoing,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    Offered,
    Dialing,
    Ringing,
    Connecting,
    Connected,
    Ending,
    Ended,
    Uncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallOffer {
    pub call_id: String,
    pub revision: u32,
    pub kind: CallKind,
    pub call_type: CallType,
    pub caller_id: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallPendingPhase {
    WaitingForReady,
    Preparing,
}
/// Metadata for a candidate that cannot be answered until ringing preparation
/// succeeds. It contains no room, application or account credentials.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallPendingOffer {
    pub call_id: String,
    pub revision: u32,
    pub kind: CallKind,
    pub call_type: CallType,
    pub caller_id: String,
    pub phase: CallPendingPhase,
}
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum CallReadyScope {
    All,
    Outgoing,
    Offer {
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(rename = "expectedRevision")]
        expected_revision: u32,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallView {
    pub call_id: String,
    pub revision: u32,
    pub kind: CallKind,
    pub call_type: CallType,
    pub direction: CallDirection,
    pub state: CallState,
    pub owned_by_this_client: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<_>")]
    pub participants: Vec<CallParticipant>,
    // Latest reported remote controls, also available to a newly connected observer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<_>")]
    pub controls: Vec<CallParticipantControl>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallParticipant {
    pub participant_id: String,
    pub contact_id: String,
    pub membership: CallMembership,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<CallParticipantStatus>,
    // Omitted until the server supplies this participant's microphone state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub microphone_enabled: Option<bool>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallMembership {
    Joined,
    Invited,
    Left,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallParticipantStatus {
    pub code: u32,
    pub kind: CallParticipantStatusKind,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallParticipantStatusKind {
    Undefined,
    Joined,
    Invited,
    Acknowledged,
    Left,
    Cancelled,
    Busy,
    Rejected,
    NoResponse,
    Unknown,
}

/// A fresh account-bound invitation locator. It carries no room credentials.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallConferenceInvitation {
    pub invitation_id: String,
    pub revision: u32,
    pub group_id: String,
    pub caller_id: String,
}

/// Application-level controls. Native protocol flags and opaque metadata stay
/// inside the daemon; a microphone update does not renegotiate the audio format.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum CallControl {
    Microphone { muted: bool },
}
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallParticipantControl {
    pub participant_id: String,
    pub control: CallControl,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallsAvailability {
    pub busy: bool,
    pub owned_by_this_client: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallSnapshot {
    #[serde(default)]
    pub conference_invitations: Vec<CallConferenceInvitation>,
    pub capabilities: CallCapabilities,
    pub availability: CallsAvailability,
    pub current_offer: Option<CallOffer>,
    #[serde(default)]
    pub current_pending_offer: Option<CallPendingOffer>,
    pub current_call: Option<CallView>,
    pub audio_format: CallAudioFormat,
}

/// Owner tokens are capability secrets. Do not log or print complete commands.
#[derive(Clone, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallCommand {
    #[serde(rename = "call.invite")]
    Invite {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(rename = "contactIds")]
        contact_ids: Vec<String>,
    },
    #[serde(rename = "call.join")]
    Join {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "invitationId")]
        invitation_id: String,
        #[serde(rename = "invitationRevision")]
        invitation_revision: u32,
    },
    #[serde(rename = "call.ready")]
    Ready {
        #[serde(rename = "requestId")]
        request_id: String,
        ready: bool,
        format: CallAudioFormat,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        scope: Option<CallReadyScope>,
    },
    #[serde(rename = "call.dial")]
    Dial {
        #[serde(rename = "requestId")]
        request_id: String,
        target: CallTarget,
        #[serde(rename = "callType")]
        call_type: CallType,
    },
    #[serde(rename = "call.answer")]
    Answer {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(rename = "expectedRevision")]
        expected_revision: u32,
    },
    #[serde(rename = "call.reject")]
    Reject {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(rename = "expectedRevision")]
        expected_revision: u32,
    },
    #[serde(rename = "call.cancel")]
    Cancel {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(rename = "expectedRevision")]
        expected_revision: u32,
    },
    #[serde(rename = "call.hangup")]
    Hangup {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(rename = "expectedRevision")]
        expected_revision: u32,
    },
    #[serde(rename = "call.mute")]
    Mute {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "callId")]
        call_id: String,
        muted: bool,
    },
    #[serde(rename = "call.clear_audio")]
    ClearAudio {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "callId")]
        call_id: String,
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
            | Self::ClearAudio { request_id, .. } => request_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallCommandStatus {
    Accepted,
    Replayed,
    Error,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallErrorCode {
    InvalidRequest,
    Busy,
    AlreadyClaimed,
    OfferExpired,
    NotOwner,
    Unsupported,
    MediaNotReady,
    OutcomeUnknown,
    Capacity,
    Stale,
    Forbidden,
    Closed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallTrackDirection {
    Send,
    Receive,
}
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallAudioTrack {
    pub id: u32,
    pub generation: u32,
    pub direction: CallTrackDirection,
    pub format: CallAudioFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub participant_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallInvitationStatus {
    Submitted,
    Rejected,
    Unknown,
    Uncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallInvitationResult {
    pub contact_id: String,
    pub status: CallInvitationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub service_code: Option<u32>,
    pub verification_required: bool,
}

/// Successful owner results include a secret ownerToken. Do not log whole events.
#[derive(Clone, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallEvent {
    #[serde(rename = "call.invitation_result")]
    InvitationResult {
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(rename = "requestId")]
        request_id: String,
        results: Vec<CallInvitationResult>,
    },
    #[serde(rename = "protocol.error")]
    ProtocolError {
        code: CallProtocolErrorCode,
        #[serde(rename = "clientVersions")]
        client_versions: Vec<u32>,
        #[serde(rename = "serverVersions")]
        server_versions: Vec<u32>,
    },
    #[serde(rename = "conference.invitations")]
    ConferenceInvitations {
        invitations: Vec<CallConferenceInvitation>,
    },
    #[serde(rename = "hello")]
    Hello {
        #[serde(rename = "conferenceInvitations", default)]
        conference_invitations: Vec<CallConferenceInvitation>,
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "clientId")]
        client_id: String,
        capabilities: CallCapabilities,
        availability: CallsAvailability,
        #[serde(rename = "currentOffer")]
        current_offer: Option<CallOffer>,
        #[serde(rename = "currentPendingOffer", default)]
        current_pending_offer: Option<CallPendingOffer>,
        #[serde(rename = "currentCall")]
        current_call: Option<CallView>,
        #[serde(rename = "audioFormat")]
        audio_format: CallAudioFormat,
    },
    #[serde(rename = "command.result")]
    CommandResult {
        #[serde(rename = "requestId")]
        request_id: String,
        status: CallCommandStatus,
        #[serde(rename = "callId")]
        call_id: Option<String>,
        #[serde(rename = "ownerToken")]
        owner_token: Option<String>,
        code: Option<CallErrorCode>,
    },
    #[serde(rename = "call.offered")]
    Offered { offer: CallOffer },
    #[serde(rename = "call.pending")]
    Pending { offer: Option<CallPendingOffer> },
    #[serde(rename = "call.state")]
    State { call: CallView },
    #[serde(rename = "participant.updated")]
    ParticipantUpdated {
        #[serde(rename = "callId")]
        call_id: String,
        revision: u32,
        participant: CallParticipant,
    },
    #[serde(rename = "call.control")]
    Control {
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(rename = "participantId")]
        participant_id: String,
        control: CallControl,
    },
    #[serde(rename = "calls.availability")]
    Availability { availability: CallsAvailability },
    #[serde(rename = "track.added")]
    TrackAdded {
        #[serde(rename = "callId")]
        call_id: String,
        track: CallAudioTrack,
    },
    #[serde(rename = "track.updated")]
    TrackUpdated {
        #[serde(rename = "callId")]
        call_id: String,
        track: CallAudioTrack,
    },
    #[serde(rename = "track.removed")]
    TrackRemoved {
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(rename = "trackId")]
        track_id: u32,
        generation: u32,
        reason: String,
    },
    #[serde(rename = "media.ready")]
    MediaReady {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "sendTrack")]
        send_track: CallAudioTrack,
        #[serde(rename = "receiveTracks")]
        receive_tracks: Vec<CallAudioTrack>,
    },
    #[serde(rename = "call.error")]
    Error { code: CallErrorCode },
}

/// JSON setup and base64 PCM on the separately authenticated media socket.
#[derive(Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallMediaCommand {
    #[serde(rename = "media.attach")]
    Attach {
        #[serde(rename = "supportedProtocolVersions")]
        supported_protocol_versions: Vec<u32>,
        #[serde(rename = "clientId")]
        client_id: String,
        #[serde(rename = "ownerToken")]
        owner_token: String,
    },
    #[serde(rename = "audio.start")]
    AudioStart {
        #[serde(rename = "trackId")]
        track_id: u32,
        generation: u32,
    },
    #[serde(rename = "audio.chunk")]
    AudioChunk {
        #[serde(rename = "trackId")]
        track_id: u32,
        generation: u32,
        #[serde(rename = "pcmBase64")]
        pcm_base64: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "number")]
        sequence: Option<u64>,
        #[serde(
            rename = "sampleOffset",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        #[ts(optional, type = "number")]
        sample_offset: Option<u64>,
    },
}
