//! Call control contracts. These types are local API messages, not remote wire schemas.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallCapabilities {
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    Direct,
    Conference,
}
/// Call identity, independent from membership and the currently negotiated media.
/// A video call can continue with audio only.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallType {
    Voice,
    Video,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallDirection {
    Incoming,
    Outgoing,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallOffer {
    pub call_id: String,
    pub revision: u32,
    pub kind: CallKind,
    pub call_type: CallType,
    pub caller_id: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallPendingPhase {
    WaitingForReady,
    Preparing,
}
/// Metadata for a candidate that cannot be answered until ringing preparation
/// succeeds. It contains no room, application or account credentials.
#[derive(Clone, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallPendingOffer {
    pub call_id: String,
    pub revision: u32,
    pub kind: CallKind,
    pub call_type: CallType,
    pub caller_id: String,
    pub phase: CallPendingPhase,
}
#[derive(Clone, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
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
#[derive(Clone, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallView {
    pub call_id: String,
    pub revision: u32,
    pub kind: CallKind,
    pub call_type: CallType,
    pub direction: CallDirection,
    pub state: CallState,
    pub owned_by_this_client: bool,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallsAvailability {
    pub busy: bool,
    pub owned_by_this_client: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallSnapshot {
    pub capabilities: CallCapabilities,
    pub availability: CallsAvailability,
    pub current_offer: Option<CallOffer>,
    #[serde(default)]
    pub current_pending_offer: Option<CallPendingOffer>,
    pub current_call: Option<CallView>,
    pub audio_format: CallAudioFormat,
}

/// Owner tokens are capability secrets. Do not log or print complete commands.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallCommand {
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallCommandStatus {
    Accepted,
    Replayed,
    Error,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallTrackDirection {
    Send,
    Receive,
}
#[derive(Clone, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallAudioTrack {
    pub id: u32,
    pub generation: u32,
    pub direction: CallTrackDirection,
    pub format: CallAudioFormat,
}

/// Successful owner results include a secret ownerToken. Do not log whole events.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallEvent {
    #[serde(rename = "hello")]
    Hello {
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
    #[serde(rename = "media.ready")]
    MediaReady {
        #[serde(rename = "sendTrack")]
        send_track: CallAudioTrack,
        #[serde(rename = "receiveTrack")]
        receive_track: CallAudioTrack,
    },
    #[serde(rename = "call.error")]
    Error { code: CallErrorCode },
}

/// JSON setup on the separately authenticated media socket. Binary messages
/// contain only PCM samples; stream identity is established by these messages.
#[derive(Serialize, Deserialize, TS)]
#[serde(tag = "type", deny_unknown_fields)]
#[ts(tag = "type")]
pub enum CallMediaCommand {
    #[serde(rename = "media.attach")]
    Attach {
        #[serde(rename = "clientId")]
        client_id: String,
        #[serde(rename = "ownerToken")]
        owner_token: String,
    },
    #[serde(rename = "audio.start")]
    AudioStart { generation: u32 },
}
