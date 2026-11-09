//! Call control contracts. These types are local API messages, not remote wire schemas.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const CALL_PROTOCOL_VERSION: u32 = 2;

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
    /// Direct video calls are available. This version never sets it.
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub direct_video: bool,
    /// Audio in conference calls is available.
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_audio: bool,
    /// Video in conference calls is available. This version never sets it.
    #[serde(default, skip_serializing_if = "capability_unavailable")]
    #[ts(optional, as = "Option<_>")]
    pub conference_video: bool,
}
fn capability_unavailable(value: &bool) -> bool {
    !value
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
/// negotiated. A video call can continue with audio only. This daemon places
/// and answers voice calls only.
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

/// A control a call participant can report. A microphone update does not
/// change the audio format.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum CallControl {
    /// The participant's microphone.
    Microphone {
        /// True when the microphone is muted.
        muted: bool,
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
    },
    /// Advertise or withdraw readiness to handle call audio. A new connection
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
    },
    /// Start an outgoing call. The socket becomes the call's owner.
    #[serde(rename = "call.dial")]
    Dial {
        /// Your ID for this command. The `command.result` repeats it.
        #[serde(rename = "requestId")]
        request_id: String,
        /// Who to call.
        target: CallTarget,
        /// Only voice calls are supported.
        #[serde(rename = "callType")]
        call_type: CallType,
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

/// Whether an audio track carries audio you send or audio you receive.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallTrackDirection {
    /// Audio from you to the call.
    Send,
    /// Audio from the call to you.
    Receive,
}
/// One audio stream: the single track you send, or one source you receive.
#[derive(Clone, Debug, Serialize, Deserialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallAudioTrack {
    /// Track ID to use in `audio.start` and `audio.chunk`.
    pub id: u32,
    /// Version of the stream. Chunks for an older generation are discarded; a
    /// new generation starts after `call.clear_audio` or a mute change.
    pub generation: u32,
    /// Whether you send or receive audio on this track.
    pub direction: CallTrackDirection,
    /// The PCM format of the track.
    pub format: CallAudioFormat,
    /// The conference participant this track carries. Omitted when the track
    /// is not tied to a participant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub participant_id: Option<String>,
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
    /// A receive audio track appeared. Audio for it follows.
    #[serde(rename = "track.added")]
    TrackAdded {
        /// ID of the call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The new track.
        track: CallAudioTrack,
    },
    /// A track moved to a new generation, for example after `call.clear_audio`
    /// or a mute change. Send new audio on the new generation.
    #[serde(rename = "track.updated")]
    TrackUpdated {
        /// ID of the call.
        #[serde(rename = "callId")]
        call_id: String,
        /// The track with its new generation.
        track: CallAudioTrack,
    },
    /// A receive track ended. Audio for that generation stops.
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
    /// Sent on the media socket after `media.attach` succeeds: the tracks to
    /// use.
    #[serde(rename = "media.ready")]
    MediaReady {
        /// The protocol version the daemon speaks on the media socket.
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        /// The track for audio you send.
        #[serde(rename = "sendTrack")]
        send_track: CallAudioTrack,
        /// The tracks for audio you receive, one per source.
        #[serde(rename = "receiveTracks")]
        receive_tracks: Vec<CallAudioTrack>,
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

/// A message on the media socket. Setup is JSON and audio is base64 PCM. Both
/// sides send `audio.chunk`: you on the send track, the daemon on receive
/// tracks. Binary messages are rejected.
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
    /// Start sending audio on your track for a generation. Send it before the
    /// first chunk, and again after each new generation.
    #[serde(rename = "audio.start")]
    AudioStart {
        /// The send track's ID.
        #[serde(rename = "trackId")]
        track_id: u32,
        /// The generation to start.
        generation: u32,
    },
    /// A chunk of PCM audio. You send it on your track; the daemon sends it on
    /// receive tracks.
    #[serde(rename = "audio.chunk")]
    AudioChunk {
        /// The track the audio belongs to.
        #[serde(rename = "trackId")]
        track_id: u32,
        /// The track generation. Chunks for an older generation are discarded.
        generation: u32,
        /// PCM16 little-endian samples as padded base64. A chunk you send is
        /// whole samples, at most 65,536 bytes once decoded.
        #[serde(rename = "pcmBase64")]
        pcm_base64: String,
        /// Order of the chunk within its source. The daemon sets it on chunks
        /// it sends; you must not send it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "number")]
        sequence: Option<u64>,
        /// Position of the chunk in per-channel samples on the shared call
        /// timeline. The daemon sets it on chunks it sends; you must not send it.
        #[serde(
            rename = "sampleOffset",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        #[ts(optional, type = "number")]
        sample_offset: Option<u64>,
    },
}
