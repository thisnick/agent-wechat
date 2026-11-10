// Generated from wechat-api/src/contracts.rs. Do not edit.
export const NATIVE_MAX_SOURCE_BYTES = 536870912 as const;
export const NATIVE_MAX_ORDINARY_SOURCE_BYTES = 26214383 as const;
export const NATIVE_MAX_VIDEO_SOURCE_BYTES = 536870912 as const;
export type CallClientHello = { "type": "call.hello", 
/**
 * One to eight distinct positive version numbers. The daemon speaks a
 * single version and answers `protocol.error` when the list does not
 * include it.
 */
supportedProtocolVersions: Array<number>, };
export type CallParticipant = { 
/**
 * ID of the participant within this call. It is not a contact ID; use it
 * to match audio tracks to participants.
 */
participantId: string, 
/**
 * Contact ID of the participant.
 */
contactId: string, 
/**
 * Whether the participant is in the room.
 */
membership: CallMembership, 
/**
 * Signaling progress, separate from membership. Omitted until reported.
 */
status?: CallParticipantStatus, 
/**
 * Whether the participant's microphone is on. Omitted until the server
 * reports it; an omitted value is unknown, not false.
 */
microphoneEnabled?: boolean, };
export type CallMembership = "joined" | "invited" | "left" | "unknown";
export type CallParticipantStatus = { 
/**
 * The status number the service reported. Keep it for codes `kind` does
 * not name.
 */
code: number, 
/**
 * What the code means.
 */
kind: CallParticipantStatusKind, };
export type CallParticipantStatusKind = "undefined" | "joined" | "invited" | "acknowledged" | "left" | "cancelled" | "busy" | "rejected" | "no_response" | "unknown";
export type CallConferenceInvitation = { 
/**
 * Opaque ID for this account. Send it in `call.join`.
 */
invitationId: string, 
/**
 * Revision of the invitation. Send it in `call.join` exactly as listed.
 */
revision: number, 
/**
 * The group the conference call is in.
 */
groupId: string, 
/**
 * Contact ID of the caller.
 */
callerId: string, };
export type CallInvitationResult = { 
/**
 * The contact this result is for.
 */
contactId: string, 
/**
 * What happened to the invitation.
 */
status: CallInvitationStatus, 
/**
 * The code the service returned. Omitted when unavailable.
 */
serviceCode?: number, 
/**
 * True when the service asked for a verification step. The event carries
 * no verification link.
 */
verificationRequired: boolean, };
export type CallInvitationStatus = "submitted" | "rejected" | "unknown" | "uncertain";
export type CallProtocolErrorCode = "unsupported_protocol_version" | "invalid_protocol_version";
export type CallCapabilities = { 
/**
 * Starting a conference call in a group is available.
 */
conferenceCreate?: boolean, 
/**
 * Inviting more contacts into a connected conference call is available.
 */
conferenceInvite?: boolean, 
/**
 * Joining a conference call from an invitation is available.
 */
conferenceJoin?: boolean, 
/**
 * Direct voice calls with one contact are available.
 */
directAudio?: boolean, 
/**
 * Direct video calls are available. This version never sets it.
 */
directVideo?: boolean, 
/**
 * Audio in conference calls is available.
 */
conferenceAudio?: boolean, 
/**
 * Video in conference calls is available. This version never sets it.
 */
conferenceVideo?: boolean, };
export type CallAudioFormat = { 
/**
 * Samples per second, per channel.
 */
sampleRate: number, 
/**
 * Number of interleaved channels.
 */
channels: number, 
/**
 * Length of one audio frame in milliseconds.
 */
frameMs: number, };
export type CallTarget = { "kind": "direct", 
/**
 * The contact to call. A group ID is not accepted.
 */
contactId: string, } | { "kind": "conference", 
/**
 * The group to call, whose ID ends in `@chatroom`.
 */
groupId: string, 
/**
 * Contacts to invite: between 1 and 100.
 */
invitees: Array<string>, };
export type CallKind = "direct" | "conference";
export type CallType = "voice" | "video";
export type CallDirection = "incoming" | "outgoing";
export type CallState = "offered" | "dialing" | "ringing" | "connecting" | "connected" | "ending" | "ended" | "uncertain";
export type CallOffer = { 
/**
 * ID of the offered call.
 */
callId: string, 
/**
 * Increases when the offer changes. Send the latest value as
 * `expectedRevision` when answering or rejecting.
 */
revision: number, 
/**
 * Whether the call is direct or a conference.
 */
kind: CallKind, 
/**
 * Whether the call is voice or video.
 */
callType: CallType, 
/**
 * Contact ID of the caller.
 */
callerId: string, };
export type CallPendingPhase = "waiting_for_ready" | "preparing";
export type CallPendingOffer = { 
/**
 * ID of the incoming call.
 */
callId: string, 
/**
 * Increases when the offer changes.
 */
revision: number, 
/**
 * Whether the call is direct or a conference.
 */
kind: CallKind, 
/**
 * Whether the call is voice or video.
 */
callType: CallType, 
/**
 * Contact ID of the caller.
 */
callerId: string, 
/**
 * What the call is waiting for.
 */
phase: CallPendingPhase, };
export type CallReadyScope = { "kind": "all" } | { "kind": "outgoing" } | { "kind": "offer", 
/**
 * ID of the pending incoming call.
 */
callId: string, 
/**
 * The offer revision you expect; a different revision is rejected.
 */
expectedRevision: number, };
export type CallView = { 
/**
 * ID of the call.
 */
callId: string, 
/**
 * Increases each time the call changes. Send the latest value as
 * `expectedRevision`.
 */
revision: number, 
/**
 * Whether the call is direct or a conference.
 */
kind: CallKind, 
/**
 * Whether the call is voice or video.
 */
callType: CallType, 
/**
 * Whether this account placed the call or received it.
 */
direction: CallDirection, 
/**
 * Where the call is in its life.
 */
state: CallState, 
/**
 * True when this connection owns the call. Always false in HTTP snapshots.
 */
ownedByThisClient: boolean, 
/**
 * The group of a conference call. Omitted for a direct call.
 */
groupId?: string, 
/**
 * The latest conference roster. Omitted for a direct call.
 */
participants?: Array<CallParticipant>, 
/**
 * The latest controls reported by the other party of a direct call, also
 * given to a newly connected observer. Omitted until one is reported.
 */
controls?: Array<CallParticipantControl>, };
export type CallControl = { "kind": "microphone", 
/**
 * True when the microphone is muted.
 */
muted: boolean, };
export type CallParticipantControl = { 
/**
 * The participant the control belongs to. In a direct call this is the
 * contact ID.
 */
participantId: string, 
/**
 * The control and its value.
 */
control: CallControl, };
export type CallsAvailability = { 
/**
 * True while this account has a call, including one whose outcome is
 * unresolved, so a new call cannot start.
 */
busy: boolean, 
/**
 * True when this connection owns that call.
 */
ownedByThisClient: boolean, };
export type CallSnapshot = { 
/**
 * Conference calls this account can join.
 */
conferenceInvitations: Array<CallConferenceInvitation>, 
/**
 * Calling operations that are available.
 */
capabilities: CallCapabilities, 
/**
 * Whether this account is in a call.
 */
availability: CallsAvailability, 
/**
 * The incoming call that can be answered now, or null.
 */
currentOffer: CallOffer | null, 
/**
 * An incoming call that cannot be answered yet, or null.
 */
currentPendingOffer: CallPendingOffer | null, 
/**
 * The active call, or null.
 */
currentCall: CallView | null, 
/**
 * The PCM format used for call audio.
 */
audioFormat: CallAudioFormat, };
export type CallCommand = { "type": "call.invite", 
/**
 * Your ID for this command. The `command.result` repeats it.
 */
requestId: string, 
/**
 * ID of the conference call.
 */
callId: string, 
/**
 * Contacts to invite: between 1 and 100.
 */
contactIds: Array<string>, } | { "type": "call.join", 
/**
 * Your ID for this command. The `command.result` repeats it.
 */
requestId: string, 
/**
 * The `invitationId` of an entry in `conferenceInvitations`.
 */
invitationId: string, 
/**
 * The `revision` of that entry. A stale or used invitation fails with
 * `offer_expired`.
 */
invitationRevision: number, } | { "type": "call.ready", 
/**
 * Your ID for this command. The `command.result` repeats it.
 */
requestId: string, 
/**
 * True to advertise readiness, false to withdraw it.
 */
ready: boolean, 
/**
 * The PCM format you will send and receive. It must match the
 * daemon's audio format.
 */
format: CallAudioFormat, 
/**
 * Which calls readiness covers. Omitted means all. Ignored when
 * withdrawing readiness.
 */
scope?: CallReadyScope, } | { "type": "call.dial", 
/**
 * Your ID for this command. The `command.result` repeats it.
 */
requestId: string, 
/**
 * Who to call.
 */
target: CallTarget, 
/**
 * Only voice calls are supported.
 */
callType: CallType, } | { "type": "call.answer", 
/**
 * Your ID for this command. The `command.result` repeats it.
 */
requestId: string, 
/**
 * ID of the offered call.
 */
callId: string, 
/**
 * The latest offer `revision` you received. A stale revision is
 * rejected.
 */
expectedRevision: number, } | { "type": "call.reject", 
/**
 * Your ID for this command. The `command.result` repeats it.
 */
requestId: string, 
/**
 * ID of the offered call.
 */
callId: string, 
/**
 * The latest offer `revision` you received. A stale revision is
 * rejected.
 */
expectedRevision: number, } | { "type": "call.cancel", 
/**
 * Your ID for this command. The `command.result` repeats it.
 */
requestId: string, 
/**
 * ID of your outgoing call.
 */
callId: string, 
/**
 * The latest call `revision` you received. A stale revision is
 * rejected.
 */
expectedRevision: number, } | { "type": "call.hangup", 
/**
 * Your ID for this command. The `command.result` repeats it.
 */
requestId: string, 
/**
 * ID of the call.
 */
callId: string, 
/**
 * The latest call `revision` you received. A stale revision is
 * rejected.
 */
expectedRevision: number, } | { "type": "call.mute", 
/**
 * Your ID for this command. The `command.result` repeats it.
 */
requestId: string, 
/**
 * ID of the call.
 */
callId: string, 
/**
 * True to mute, false to unmute.
 */
muted: boolean, } | { "type": "call.clear_audio", 
/**
 * Your ID for this command. The `command.result` repeats it.
 */
requestId: string, 
/**
 * ID of the call.
 */
callId: string, };
export type CallCommandStatus = "accepted" | "replayed" | "error";
export type CallErrorCode = "invalid_request" | "busy" | "already_claimed" | "offer_expired" | "not_owner" | "unsupported" | "media_not_ready" | "outcome_unknown" | "capacity" | "stale" | "forbidden" | "closed";
export type CallTrackDirection = "send" | "receive";
export type CallAudioTrack = { 
/**
 * Track ID to use in `audio.start` and `audio.chunk`.
 */
id: number, 
/**
 * Version of the stream. Chunks for an older generation are discarded; a
 * new generation starts after `call.clear_audio` or a mute change.
 */
generation: number, 
/**
 * Whether you send or receive audio on this track.
 */
direction: CallTrackDirection, 
/**
 * The PCM format of the track.
 */
format: CallAudioFormat, 
/**
 * The conference participant this track carries. Omitted when the track
 * is not tied to a participant.
 */
participantId?: string, };
export type CallEvent = { "type": "call.invitation_result", 
/**
 * ID of the conference call.
 */
callId: string, 
/**
 * The `requestId` of the create or invite command.
 */
requestId: string, 
/**
 * One result per invited contact.
 */
results: Array<CallInvitationResult>, } | { "type": "protocol.error", 
/**
 * Why the versions were refused.
 */
code: CallProtocolErrorCode, 
/**
 * The versions the client announced. Empty when the list was invalid.
 */
clientVersions: Array<number>, 
/**
 * The versions the daemon supports.
 */
serverVersions: Array<number>, } | { "type": "conference.invitations", 
/**
 * The current invitations.
 */
invitations: Array<CallConferenceInvitation>, } | { "type": "hello", 
/**
 * Conference calls this account can join.
 */
conferenceInvitations: Array<CallConferenceInvitation>, 
/**
 * The protocol version the daemon speaks on this socket.
 */
protocolVersion: number, 
/**
 * ID of this connection. Send it as `clientId` in `media.attach`.
 */
clientId: string, 
/**
 * Calling operations that are available.
 */
capabilities: CallCapabilities, 
/**
 * Whether this account is in a call.
 */
availability: CallsAvailability, 
/**
 * The incoming call that can be answered now, or null.
 */
currentOffer: CallOffer | null, 
/**
 * An incoming call that cannot be answered yet, or null.
 */
currentPendingOffer: CallPendingOffer | null, 
/**
 * The active call, or null.
 */
currentCall: CallView | null, 
/**
 * The PCM format used for call audio.
 */
audioFormat: CallAudioFormat, } | { "type": "command.result", 
/**
 * The `requestId` of the command.
 */
requestId: string, 
/**
 * Whether the command was accepted, repeated or rejected.
 */
status: CallCommandStatus, 
/**
 * The call the command claimed. Null unless it was a dial, answer or
 * join that succeeded.
 */
callId: string | null, 
/**
 * Secret token for `media.attach` on the claimed call. Null unless a
 * call was claimed. Do not log it.
 */
ownerToken: string | null, 
/**
 * Why the command failed. Null unless `status` is `error`.
 */
code: CallErrorCode | null, } | { "type": "call.offered", 
/**
 * The offer. Send its `revision` in `call.answer` or `call.reject`.
 */
offer: CallOffer, } | { "type": "call.pending", 
/**
 * The pending call, or null to clear it.
 */
offer: CallPendingOffer | null, } | { "type": "call.state", 
/**
 * The call as it is now.
 */
call: CallView, } | { "type": "participant.updated", 
/**
 * ID of the conference call.
 */
callId: string, 
/**
 * The call revision after this change.
 */
revision: number, 
/**
 * The participant as it is now.
 */
participant: CallParticipant, } | { "type": "call.control", 
/**
 * ID of the direct call.
 */
callId: string, 
/**
 * The contact whose control changed.
 */
participantId: string, 
/**
 * The control and its new value.
 */
control: CallControl, } | { "type": "calls.availability", 
/**
 * The new availability.
 */
availability: CallsAvailability, } | { "type": "track.added", 
/**
 * ID of the call.
 */
callId: string, 
/**
 * The new track.
 */
track: CallAudioTrack, } | { "type": "track.updated", 
/**
 * ID of the call.
 */
callId: string, 
/**
 * The track with its new generation.
 */
track: CallAudioTrack, } | { "type": "track.removed", 
/**
 * ID of the call.
 */
callId: string, 
/**
 * ID of the track that ended.
 */
trackId: number, 
/**
 * The generation that ended.
 */
generation: number, 
/**
 * Why the track ended. Currently `source_unavailable`.
 */
reason: string, } | { "type": "media.ready", 
/**
 * The protocol version the daemon speaks on the media socket.
 */
protocolVersion: number, 
/**
 * The track for audio you send.
 */
sendTrack: CallAudioTrack, 
/**
 * The tracks for audio you receive, one per source.
 */
receiveTracks: Array<CallAudioTrack>, } | { "type": "call.error", 
/**
 * What went wrong.
 */
code: CallErrorCode, };
export type CallMediaCommand = { "type": "media.attach", 
/**
 * The protocol versions you support.
 */
supportedProtocolVersions: Array<number>, 
/**
 * The `clientId` from the control socket's `hello`.
 */
clientId: string, 
/**
 * The secret `ownerToken` from the `command.result` that claimed the
 * call. Do not log it.
 */
ownerToken: string, } | { "type": "audio.start", 
/**
 * The send track's ID.
 */
trackId: number, 
/**
 * The generation to start.
 */
generation: number, } | { "type": "audio.chunk", 
/**
 * The track the audio belongs to.
 */
trackId: number, 
/**
 * The track generation. Chunks for an older generation are discarded.
 */
generation: number, 
/**
 * PCM16 little-endian samples as padded base64. A chunk you send is
 * whole samples, at most 65,536 bytes once decoded.
 */
pcmBase64: string, 
/**
 * Order of the chunk within its source. The daemon sets it on chunks
 * it sends; you must not send it.
 */
sequence?: number, 
/**
 * Position of the chunk in per-channel samples on the shared call
 * timeline. The daemon sets it on chunks it sends; you must not send it.
 */
sample_offset?: number, };
export type NativeMentions = { 
/**
 * `known` when the mention list was read, `unknown` when it is missing or cannot be parsed.
 * When unknown, `userIds` is empty and the flags are null.
 */
state: string, 
/**
 * IDs of the members mentioned by name, not counting the mention-everyone marker (see `all`).
 */
userIds: Array<string>, 
/**
 * True when this account is among the mentioned members, false when it is not, and null when
 * the mentions are unknown.
 */
self: boolean | null, 
/**
 * True when the message mentions everyone, false when it does not, and null when the mentions
 * are unknown.
 */
all: boolean | null, 
/**
 * Where the mentions were read from, currently `msgsource.atuserlist`. Null when there is no
 * source, for example when `state` is `unknown`.
 */
source: string | null, };
export type NativeQuoteReference = { 
/**
 * WeChat's ID of the quoted message, as a string.
 */
serverId: string, 
/**
 * WeChat's numeric type of the quoted message, with the same values as `type` on a message.
 */
type: number, 
/**
 * How much of the quote could be shown: `embedded` when the reply carries readable content,
 * `missing` when it carries none, `unsupported` for a kind of message that cannot be shown as
 * a quote, and `recalled` when the original was recalled and its content is hidden.
 */
state?: string, 
/**
 * ID of the quoted message's sender, when the reply says who it was.
 */
senderId?: string, 
/**
 * Sender's display name carried inside the reply, when supplied by WeChat.
 */
senderName?: string, 
/**
 * `outgoing` when the quoted message was sent by this account and `incoming` when someone else
 * sent it. Null when that cannot be told.
 */
direction?: string, 
/**
 * App subtype of the quoted message when it is an app message (type 49), such as 5 for a link.
 * Null for other messages.
 */
appMessageType?: number, 
/**
 * Readable text of the quoted message, taken from the reply. Null when there is none.
 */
embeddedText?: string, 
/**
 * True when the reply carries the quoted image, so it can be downloaded through the reply's
 * media routes.
 */
embeddedImage?: boolean, 
/**
 * Link card of the quoted message when it was a shared link, with the same fields as `link` on
 * a message.
 */
embeddedLink?: LinkCard, 
/**
 * Mini Program metadata retained by the reply.
 */
embeddedMiniProgram?: MiniProgramCard, 
/**
 * True when the quoted message is a sticker. Omitted otherwise.
 */
embeddedSticker?: boolean, };
export type LinkCard = { 
/**
 * Card title. Null when there is none.
 */
title: string | null, 
/**
 * Card description or summary. Null when there is none.
 */
description: string | null, 
/**
 * Address the card opens. Null when the card has none or it is not a valid http or https
 * address.
 */
url: string | null, 
/**
 * Name of the site or account that published the card, such as a Channels account. Null when
 * there is none.
 */
sourceName: string | null, };
export type LocationCoordinateSystem = "unknown";
export type LocationCard = { 
/**
 * Latitude in degrees, from -90 to 90.
 */
latitude: number, 
/**
 * Longitude in degrees, from -180 to 180.
 */
longitude: number, 
/**
 * Place name supplied with the pin. Null when there is none.
 */
name: string | null, 
/**
 * Address text supplied with the pin. Null when there is none.
 */
address: string | null, 
/**
 * Map scale value supplied with the pin, as sent. Null when there is none.
 */
scale: number | null, 
/**
 * Map type text supplied with the pin, as sent. Null when there is none.
 */
mapType: string | null, 
/**
 * Place identifier supplied with the pin, as sent. Null when there is none.
 */
poiId: string | null, 
/**
 * Coordinate system of the numbers. It is always `unknown`, because coordinates are reported
 * as sent.
 */
coordinateSystem: LocationCoordinateSystem, };
export type NativeRecallInfo = { 
/**
 * When the daemon first saw the recall, as an RFC 3339 time.
 */
observedAt: string, 
/**
 * When WeChat says the message was recalled, as an RFC 3339 time. Omitted when that time is
 * not known.
 */
recalledAt?: string, 
/**
 * The notice text WeChat saved in place of the message. Omitted or null when there is none.
 */
replacementText?: string | null, };
export type NativeMessage = { 
/**
 * Message ID. Pass it back unchanged to routes that take a message ID.
 */
id: string, 
/**
 * ID of the account the message belongs to.
 */
accountId: string, 
/**
 * ID of the chat the message is in.
 */
threadId: string, 
/**
 * WeChat's own ID for the message, as a string because it can exceed the largest whole number
 * JavaScript holds exactly. Null when the saved message has none.
 */
serverId: string | null, 
/**
 * ID of the sender. Null for system notices and when the sender cannot be determined.
 */
senderId: string | null, 
/**
 * WeChat's numeric message type, such as 1 text, 3 image, 34 voice, 43 video, 47 sticker, 48
 * location, 49 app message (links, files, quotes) and 10000 system notice.
 */
type: number, 
/**
 * For app messages (type 49), the app subtype, such as 5 link, 6 file, 8 shared sticker, 19
 * forwarded chat and 57 quote reply. Null for other messages.
 */
appMessageType?: number, 
/**
 * The message this one replies to, when it is a quote reply. Null otherwise.
 */
quote?: NativeQuoteReference, 
/**
 * Link card details when the message is a shared link, article, music or video card, or a
 * Channels post. Null otherwise.
 */
link?: LinkCard, 
/**
 * Mini Program card read from the saved message.
 */
miniProgram?: MiniProgramCard, 
/**
 * Pin details when the message is a static location. Omitted for other messages.
 */
location?: LocationCard, 
/**
 * Recall details. Present only when the message was recalled, in which case `body` is
 * `[Message recalled]`.
 */
recall?: NativeRecallInfo, 
/**
 * `outgoing` when this account sent the message and `incoming` when someone else did. Null for
 * system notices and when it cannot be determined.
 */
direction: string | null, 
/**
 * Readable text of the message, such as the text itself or a summary of a link, location or
 * forwarded chat. Null when there is nothing readable, for example for a photo.
 */
body: string | null, 
/**
 * When the message was created, as an RFC 3339 time. Null when the saved time is missing.
 */
createdAt: string | null, 
/**
 * Which members the message mentions.
 */
mentions: NativeMentions, 
/**
 * Fingerprint of the saved profile data this message was read from. It changes whenever the
 * profile files change, not only when this message changes.
 */
version: string, 
/**
 * Always an empty list today. Use the message's media routes to list its attachments.
 */
media: unknown[], };
export type PeerCategory = "unknown" | "system" | "group" | "business" | "openim" | "openim_customer_service";
export type PeerNamespace = "wechat" | "openim" | "openim_customer_service" | "im_chatroom" | "qy" | "weclaw" | "other";
export type PeerBusinessKind = "official_account" | "service_account" | "enterprise" | "enterprise_child" | "photo_account";
export type PeerWording = { 
/**
 * The language of the text, as the number WeChat stores. Choose the language to show in your
 * app.
 */
languageId: number, 
/**
 * The label text.
 */
text: string, };
export type PeerApplication = { 
/**
 * The language this entry applies to, as the number WeChat stores.
 */
languageId: number, 
/**
 * ID of the account type for this language. Null when none is saved.
 */
accountTypeId: string | null, 
/**
 * True when the profile also holds the label for this account type and language. The label
 * itself is not returned.
 */
accountTypeCached: boolean, };
export type PeerOpenIm = { 
/**
 * ID of the WeCom application the peer belongs to. Null when none is saved.
 */
appId: string | null, 
/**
 * ID of the set of labels in `wordings`. Null when none is saved.
 */
wordingId: string | null, 
/**
 * Localized labels for the peer, one per language.
 */
wordings: Array<PeerWording>, 
/**
 * Account type references, one per language.
 */
applications: Array<PeerApplication>, };
export type PeerClassification = { category: PeerCategory, namespace: PeerNamespace, 
/**
 * Numeric service type saved for a business account: 0 official account, 1 service account, 2
 * enterprise, 3 enterprise child, 5 photo account. Null for other peers and when the saved
 * values conflict.
 */
businessType: number | null, businessKind: PeerBusinessKind | null, 
/**
 * Saved WeCom details, present for `openim` and `openim_customer_service` peers when the
 * profile has them. Null otherwise.
 */
openIm: PeerOpenIm | null, };
export type NativeThread = { 
/**
 * Chat ID. For a direct chat it is the contact's username, and for a group it ends with
 * `@chatroom`.
 */
id: string, 
/**
 * ID of the account the chat belongs to.
 */
accountId: string, 
/**
 * ID of the person or group on the other side of the chat. It is the same as `id`.
 */
peerId: string, 
/**
 * `direct` for a one-to-one chat, `group` for a group, and `system` for a built-in chat such
 * as File Transfer.
 */
kind: string, classification?: PeerClassification, 
/**
 * Name shown for the chat: your remark if set, else the contact's nickname or the group's
 * name, else the ID.
 */
displayName: string | null, 
/**
 * Number of unread messages as saved in the profile. Null when the saved data does not say.
 */
unreadCount: number | null, 
/**
 * Saved notification preference: true when the chat is muted and false when it is not. Null
 * means the profile cannot establish it.
 */
muted?: boolean | null, 
/**
 * Saved pin preference: true when the chat is pinned and false when it is not. Null means the
 * profile cannot establish it.
 */
pinned?: boolean | null, 
/**
 * Version of the chat's read state. Send it as `expectedVersion` when marking the chat read;
 * the request is refused if the chat changed since.
 */
readStateVersion: string, 
/**
 * Preview of the latest message. Omitted when local message data is unavailable, and null when
 * the chat has no messages.
 */
lastMessage?: NativeThreadMessage | null, 
/**
 * Up to four cached group members, for drawing the group's picture. Empty for direct chats or
 * when the member list is unavailable.
 */
avatarMembers?: Array<NativeThreadAvatarMember>, 
/**
 * How complete the saved history is. It currently always reports partial coverage and no local
 * message count.
 */
history: unknown, };
export type NativeThreadMessage = { 
/**
 * ID of the message, the same as `id` on the full message.
 */
id: string, 
/**
 * ID of the sender. Null for system notices and when the sender is unknown.
 */
senderId: string | null, 
/**
 * Name of the sender, taken from the group's cached members or your contacts. Null when the
 * message has no sender or the name is unknown.
 */
senderName: string | null, 
/**
 * `outgoing` when this account sent the message and `incoming` when someone else did. Null
 * when it cannot be determined.
 */
direction: string | null, 
/**
 * WeChat's numeric message type, with the same values as `type` on a message.
 */
type: number, 
/**
 * Plain-text preview of up to 160 characters. Media messages use labels such as Photo and
 * Sticker, and a recalled message shows its recall notice.
 */
text: string, 
/**
 * When the message was created, as an RFC 3339 time. Null when unknown.
 */
createdAt: string | null, };
export type NativeThreadAvatarMember = { 
/**
 * Member ID.
 */
id: string, 
/**
 * Name to show for the member.
 */
displayName: string, };
export type NativeContact = { 
/**
 * Contact ID. It is the same as `username`.
 */
id: string, 
/**
 * The contact's WeChat username.
 */
username: string, 
/**
 * Name to show: the remark if set, else the nickname, else the username.
 */
displayName: string | null, 
/**
 * The name the contact chose for themselves. Null when none is saved.
 */
nickname: string | null, 
/**
 * Your private name for the contact. Null when none is saved.
 */
remark: string | null, 
/**
 * The custom WeChat ID the contact chose, which they can change. Null when none is saved.
 */
alias: string | null, 
/**
 * Relationship to this account. It currently always reports `unknown`, including for saved
 * friends.
 */
relation: string, classification?: PeerClassification, };
export type NativeGroup = { 
/**
 * Group ID, ending with `@chatroom`. It is the same as `roomId`.
 */
id: string, 
/**
 * Group ID, the same as `id`.
 */
roomId: string, classification?: PeerClassification, 
/**
 * Name to show for the group: your remark if set, else the group's name, else the ID.
 */
displayName: string | null, 
/**
 * ID of the member who owns the group. Null when the saved data does not say.
 */
ownerId: string | null, 
/**
 * IDs of the group's administrators. The owner is not listed unless WeChat lists them too, and
 * the field is null when the saved data cannot say.
 */
administratorIds?: Array<string> | null, 
/**
 * Whether this account may mention everyone: true for the owner and administrators and false
 * for other members. Null means permission is not established, and older servers omit the
 * field.
 */
canMentionAll?: boolean | null, 
/**
 * Text of the group's current announcement. Null when there is none.
 */
announcement: string | null, 
/**
 * The group's own shared name. Null when none is saved.
 */
name?: string | null, 
/**
 * Your private name for the group. Null when none is saved.
 */
remark?: string | null, 
/**
 * True when notifications for the group are muted and false when they are on. Null when the
 * saved data does not say.
 */
muted?: boolean | null, 
/**
 * True when the group is minimized (folded) in the chat list and false when it is not. Null
 * when the saved data does not say.
 */
minimized?: boolean | null, 
/**
 * True when this account is still a member of the group and false when it is not. Null when
 * the saved data does not say.
 */
isInGroup?: boolean | null, 
/**
 * This account's own display name in the group. Null or omitted when it is not known.
 */
myDisplayName?: string | null, };
export type NativeGroupMember = { 
/**
 * Member ID. It is the same as `username`.
 */
id: string, 
/**
 * The member's WeChat username.
 */
username: string, classification?: PeerClassification, 
/**
 * Name to show for the member: your remark, else their name in this group, else their
 * nickname, else their username.
 */
displayName: string | null, 
/**
 * The member's own display name in this group. Null when none is saved.
 */
roomDisplayName: string | null, 
/**
 * Name used for this member in mention text. It comes from their name in the group or their
 * nickname, never from your private remark, and falls back to the username.
 */
mentionName?: string, };
export type NativeGroupMemberLookup = { 
/**
 * ID of the group that was searched.
 */
groupId: string, 
/**
 * Username that was looked up.
 */
username: string, 
/**
 * `present` when the person is in the saved member list and `unconfirmed` when they are not. A
 * missing row does not prove they left, because the saved list can be incomplete.
 */
state: string, 
/**
 * The member's details when `state` is `present`, otherwise null.
 */
member: NativeGroupMember | null, 
/**
 * How the answer was obtained. It is currently always `cached`, meaning saved data only.
 */
coverage: string, };
export type NativeGroupCreateRequest = { 
/**
 * IDs of the contacts to add, from 2 to 40, without repeats and without this account's own ID.
 */
members: Array<string>, 
/**
 * Name for the new group, up to 512 bytes. Optional.
 */
name?: string, };
export type NativeGroupNameRequest = { 
/**
 * The new group name. It must not be blank and can be up to 512 bytes.
 */
name: string, };
export type NativeGroupMembersRequest = { 
/**
 * IDs of the contacts, from 1 to 40, without repeats and without this account's own ID.
 */
members: Array<string>, };
export type NativeGroupMyNameRequest = { 
/**
 * The new display name, up to 512 bytes.
 */
displayName: string, };
export type NativeThreadNotificationsRequest = { 
/**
 * True to mute the chat and false to turn its notifications back on.
 */
muted: boolean, };
export type NativeThreadPinRequest = { 
/**
 * True to pin the chat and false to unpin it.
 */
pinned: boolean, };
export type NativeTombstone = { 
/**
 * ID of the removed resource.
 */
id: string, 
/**
 * Always true, marking the resource as removed.
 */
deleted: true, 
/**
 * Fingerprint of the saved profile data in which the resource is missing, the same value as
 * `sourceGeneration` on the page.
 */
version: string, };
export type NativePageItem<T> = T | NativeTombstone;
export type NativePage<T> = { 
/**
 * The entries on this page, in list order.
 */
items: Array<NativePageItem<T>>, 
/**
 * Pass it as `cursor` with the same query to get the next page. Null on the last page.
 */
nextCursor: string | null, 
/**
 * Which saved databases the read covered, as `partial` (true when some could not be read) and
 * `databases` (each one with its state). It describes local data, not completeness on WeChat's
 * servers.
 */
coverage: unknown, 
/**
 * Fingerprint of the saved profile data the page was read from. It changes when the profile
 * files change.
 */
sourceGeneration: string, 
/**
 * Event cursor that was current when the list was first read. Resume the event stream from it
 * to catch changes made after the read.
 */
asOfEventCursor?: string, };
export type MiniProgramCard = { 
/**
 * Public Mini Program AppID.
 */
appId: string, 
/**
 * Program name, when supplied by the card.
 */
displayName: string | null, 
/**
 * Shared page title.
 */
title: string | null, 
/**
 * Page route and query carried in the card.
 */
pagePath: string | null, };
export type NativeMiniProgram = { 
/**
 * Public Mini Program AppID.
 */
appId: string, 
/**
 * Name shown in the known-app list.
 */
title: string, 
/**
 * Whether the hub has a staged package for this program.
 */
available: boolean, };
export type NativeMiniPrograms = { 
/**
 * Known apps and their package availability.
 */
items: Array<NativeMiniProgram>, };
export type NativeMiniProgramEntry = { 
/**
 * Page route within the package.
 */
path: string, 
/**
 * Archive path of the compiled page's service module.
 */
servicePath: string, 
/**
 * Query string of the shared entry, without its leading question mark.
 */
query: string, 
/**
 * Compiled component framework selected by the runner.
 */
framework: string, 
/**
 * Renderer used by the local runner.
 */
renderer: string, };
export type NativeMiniProgramPackage = { 
/**
 * Package ID to use when downloading this archive.
 */
id: string, 
/**
 * Exact cached version to request.
 */
version: string, 
/**
 * Byte length of the original archive.
 */
sizeBytes: number, 
/**
 * SHA-256 of the original archive, in lowercase hex.
 */
sha256: string, 
/**
 * Declared root for a cached subpackage. Omitted for the main archive.
 */
root?: string, };
export type NativeMiniProgramCapabilities = { 
/**
 * Whether the runner may request app login codes.
 */
login: boolean, 
/**
 * Whether this launch can share its current page to a chat.
 */
shareToChat: boolean, };
export type NativeMiniProgramLaunchRequest = { 
/**
 * Saved card message ID. Omit to use the program's default page.
 */
sourceMessageId?: string, };
export type NativeMiniProgramLaunch = { 
/**
 * Temporary launch ID bound to the initiating account and token.
 */
launchId: string, 
/**
 * Page selected for the local runner.
 */
entry: NativeMiniProgramEntry, 
/**
 * Verified archives to download before execution.
 */
packages: Array<NativeMiniProgramPackage>, 
/**
 * Operations the runner can request for this launch.
 */
capabilities: NativeMiniProgramCapabilities, 
/**
 * Current server policy, or null when the account is offline.
 */
networkPolicy: NativeMiniProgramNetworkPolicy | null, };
export type NativeMiniProgramNetworkPolicy = { 
/**
 * Domain patterns used by the host's request-domain check.
 */
requestDomains: Array<string>, };
export type NativeMiniProgramIpcRequest = { "method": "wx.login", };
export type NativeMiniProgramLogin = { 
/**
 * WeChat code for the selected app's backend. Treat it as sensitive.
 */
code: string, };
export type NativeOperation = { 
/**
 * Operation ID.
 */
id: string, 
/**
 * ID of the account the operation belongs to.
 */
accountId: string, 
/**
 * What the operation does, such as `login`, `logout`, `sync`, `metadata`, `group_mutation`,
 * `message_recall` or `media-writeback`.
 */
kind: string, 
/**
 * Where the operation stands, such as `queued`, `running`, `succeeded`, `failed` or
 * `cancelled`. Some kinds report more states, for example `needs_review`, or `awaiting_scan`
 * and `awaiting_approval` during a login.
 */
state: string, 
/**
 * Details of the outcome. Its fields depend on `kind`, and it is empty until the operation has
 * something to report.
 */
result: unknown, 
/**
 * When the operation was created, as an RFC 3339 time.
 */
createdAt: string, 
/**
 * When the operation last changed, as an RFC 3339 time.
 */
updatedAt: string, };
export type NativeSendIntent = { 
/**
 * Send ID, used to look the send up or cancel it.
 */
id: string, 
/**
 * ID of the account that is sending.
 */
accountId: string, 
/**
 * ID of the chat the message is sent to.
 */
destination: string, 
/**
 * `queued`, `submitting`, `accepted` (WeChat took it), `failed` (WeChat refused it),
 * `needs_review` (the outcome is unknown, so check the chat before sending again) or
 * `cancelled`.
 */
state: string, 
/**
 * ID the daemon gave the message when it submitted it to WeChat.
 */
clientId: string | null, 
/**
 * WeChat's ID of the message once it was accepted, as a string. Null until then.
 */
serverId: string | null, 
/**
 * Why the send failed or needs review. Null otherwise.
 */
errorCode: string | null, 
/**
 * ID of the saved message that represents this send, once it appears in the profile. Omitted
 * until then.
 */
localMessageId?: string, 
/**
 * When the send was created, as an RFC 3339 time.
 */
createdAt: string, };
export type NativeEvent = { 
/**
 * Position of the event in the log, a decimal number written as a string that only increases.
 * Pass the last one you handled as `after` to resume.
 */
cursor: string, 
/**
 * Kind of event, such as `message.committed`, `thread.updated`, `group.updated`,
 * `operation.updated` or `send.updated`.
 */
kind: string, 
/**
 * Details of the event. The fields depend on `kind`; for example `message.committed` carries
 * `messageId` and `threadId`.
 */
data: Record<string, unknown>, 
/**
 * When the event was recorded, as an RFC 3339 time.
 */
createdAt: string, };
export type NativeIdentity = { 
/**
 * Product name, `agent-wechat`.
 */
product: string, 
/**
 * Backend in use, normally `native`.
 */
backend: string, 
/**
 * Version of the daemon.
 */
version: string, 
/**
 * Version of this HTTP API, currently 1.
 */
apiVersion: number, 
/**
 * Platform the daemon was built for, as a Rust target triple such as
 * `x86_64-unknown-linux-gnu`.
 */
target?: string, 
/**
 * Names of the features this daemon supports, such as `text-send` and `events`.
 */
capabilities: Array<string>, };
export type NativeStatus = { 
/**
 * ID of this daemon's data directory. It is created once and kept across restarts.
 */
serverId: string, 
/**
 * Path of the data directory on the daemon's host.
 */
dataDir: string, 
/**
 * Process ID of the daemon.
 */
pid: number, 
/**
 * Random ID created each time the daemon starts. A shutdown request must repeat it.
 */
ownershipGeneration: string, 
/**
 * Identity and build of the daemon.
 */
version: NativeIdentity, 
/**
 * Daemon state. It is `ready` while the daemon is serving requests.
 */
state: string, };
export type AccountSettings = { 
/**
 * Largest advertised size in bytes that the daemon downloads automatically for this account,
 * from 0 to 536870912. Thumbnails and voice notes are always downloaded, and 0 limits
 * automatic downloads to those; the default is 52428800.
 */
autoDownloadMaxBytes: number, };
export type NativeAccountRemovalRequest = { 
/**
 * Must repeat the account ID from the URL, to confirm the removal.
 */
confirmAccountId: string, };
export type NativeAccountRemoval = { 
/**
 * ID of the account that was removed.
 */
accountId: string, 
/**
 * True when the account was removed.
 */
removed: boolean, 
/**
 * True when the account's local files were kept. They are always kept.
 */
filesRetained: boolean, };
export type QuoteSendReference = { 
/**
 * ID of the saved message to quote, copied from `id` on the message.
 */
messageId: string, 
/**
 * The quoted message's `serverId`, copied as a string. The request is refused if it does not
 * match the saved message.
 */
serverId: string, };
export type TextMentionPosition = { 
/**
 * UTF-16 offset in the text where the mention starts, at the `@` character.
 */
start: number, 
/**
 * UTF-16 offset just past the end of the mention. The mention includes its closing space
 * character (U+2005).
 */
end: number, 
/**
 * Which `userIds` entry this mention stands for, counting from 0. Use 0 when mentioning
 * everyone.
 */
index: number, };
export type TextMentions = { 
/**
 * IDs of the members to mention, at most 128 and without repeats. Leave out when `all` is
 * true.
 */
userIds?: Array<string>, 
/**
 * True to mention everyone. Only the group owner and administrators can do this.
 */
all?: boolean, 
/**
 * Where each mention sits inside `text`, so it shows inline. Without positions the names are
 * placed before the text, and when given, every mentioned member needs at least one position.
 */
positions?: Array<TextMentionPosition>, };
export type NativeTextSendRequest = { 
/**
 * ID of the chat to send to.
 */
threadId: string, 
/**
 * The message text. A quote reply can hold up to 16 KiB and other text up to 64 KiB, including
 * mention names.
 */
text: string, 
/**
 * The saved message this text replies to. The token also needs the `read` scope.
 */
quote?: QuoteSendReference, 
/**
 * Group members to mention. Only allowed when sending to a group.
 */
mentions?: TextMentions, };
export type ShareSendRequest = { "kind": "share", 
/**
 * ID of the chat to send to.
 */
threadId: string, 
/**
 * Card title. It must not be blank.
 */
title: string, 
/**
 * Address the card opens. It must be an http or https address.
 */
url: string, 
/**
 * Text shown under the title. Optional.
 */
description?: string, 
/**
 * Address of a preview image for the card. It must be an http or https address without a
 * port or fragment.
 */
thumbnailUrl?: string, };
export type NativeRecallRequest = { 
/**
 * The message's `serverId`, copied as a string. The recall is refused if it does not match the
 * message in the URL.
 */
serverId: string, };
export type NativeReadRequest = { 
/**
 * The chat's `readStateVersion` from when you read it. The request is refused if the chat has
 * changed since.
 */
expectedVersion: string, };
export type NativeForgetRequest = { 
/**
 * Must be true. It confirms that the credentials should be deleted.
 */
confirm: boolean, };
export type NativeMediaWritebackRequest = { 
/**
 * How many saved media records to repair in this request, from 1 to 100.
 */
limit: number, };
export type NativeMediaJobRetryRequest = { 
/**
 * True to reuse the upload a send job already finished instead of uploading again. The
 * retry-completed-upload route requires true here.
 */
reuseCompletedUpload?: boolean, };
export type OfficialProfileRequest = { 
/**
 * Absolute path on the daemon's host to a home folder containing `xwechat_files`, to that
 * folder, or to one account's folder. When omitted, the daemon searches only its own home
 * folder.
 */
path: string | null, 
/**
 * ID of the account expected to own this profile. The request is refused if the profile is not
 * already attached to that account.
 */
accountId: string | null, };
export type OfficialProfileSelection = { 
/**
 * ID of the account that uses the profile.
 */
accountId: string, 
/**
 * Path of the attached profile on the daemon's host.
 */
profilePath: string, 
/**
 * `official`, meaning the profile is used in place and not copied.
 */
storageMode: string, 
/**
 * True when the profile holds saved login details that can sign in without a QR code.
 */
savedDeviceLoginAvailable: boolean, };
export type LoginStrategy = "auto" | "saved-device" | "qr";
export type QrRefreshMode = "auto" | "manual";
export type QrRefresh = { 
/**
 * Whether an expired QR code that nobody scanned is replaced automatically. The default is
 * `auto`.
 */
mode: QrRefreshMode, 
/**
 * How many times to replace an expired QR code automatically, from 0 to 3. The default is 3.
 */
maxRefreshes: number, };
export type LoginRequest = { 
/**
 * How to sign in. The default is `auto`.
 */
strategy: LoginStrategy, 
/**
 * How an expired QR code is replaced.
 */
qrRefresh: QrRefresh, 
/**
 * Whether a QR code may be shown. When false, only a saved login can be used. The default is
 * true.
 */
allowQr: boolean, 
/**
 * True to skip the saved login and use a QR code, the same as strategy `qr`. The default is
 * false.
 */
forceQr: boolean, 
/**
 * How long to wait for the sign-in to finish, from 10 to 900 seconds. The default is 300.
 */
timeoutSeconds: number, 
/**
 * Name this computer gives WeChat while signing in, up to 100 characters. The default is
 * `agent-wechat`.
 */
computerName: string, };
export type MediaKind = "image" | "file" | "video" | "voice" | "sticker";
export type MediaVariant = "best" | "thumbnail" | "standard" | "original" | "normal" | "encoded";
export type VideoSendQuality = "normal" | "original";
export type MediaTransferRequest = { 
/**
 * ID of the message to download from. It must match the message ID in the URL.
 */
messageId: string, 
/**
 * ID of the specific attachment to download. When null, the daemon picks the attachment that
 * matches `quality`.
 */
assetId: string | null, 
/**
 * Which copy to download. The default is `best`.
 */
quality: MediaVariant, };
export type MediaSendRequest = { 
/**
 * ID of the chat to send to.
 */
threadId: string, 
/**
 * What to send. Stickers cannot be sent.
 */
kind: MediaKind, 
/**
 * ID of the uploaded source to send, as returned by the upload route.
 */
sourceId: string, 
/**
 * ID of an uploaded JPEG to use as the standard-size copy of an image instead of generating
 * one. Only valid for image sends.
 */
standardSourceId: string | null, 
/**
 * ID of an uploaded JPEG to use as the thumbnail instead of generating one. Only valid for
 * image and video sends.
 */
thumbnailSourceId: string | null, 
/**
 * Length of a video in seconds, from 1 to 3600. Optional, because the daemon measures the
 * video itself.
 */
playSeconds: number | null, 
/**
 * Length of a voice note in milliseconds. Optional, because the daemon measures the audio
 * itself.
 */
durationMs: number | null, 
/**
 * How to send a video, `normal` by default or `original`. Only valid for video sends.
 */
videoQuality?: VideoSendQuality, };
export type MediaImagePreview = { 
/**
 * SHA-256 of the original image bytes this preview was made from.
 */
sourceSha256: string, 
/**
 * MIME type of the preview.
 */
contentType: string, 
/**
 * Size of the preview in bytes.
 */
byteLength: number, 
/**
 * SHA-256 of the preview bytes, as lowercase hex.
 */
sha256: string, 
/**
 * File name of the preview.
 */
filename: string, 
/**
 * Path on the daemon to fetch the preview from.
 */
contentUrl: string, };
export type MediaAsset = { 
/**
 * Attachment ID, used to fetch or request this copy.
 */
id: string, 
/**
 * ID of the account the attachment belongs to.
 */
accountId: string, 
/**
 * ID of the message the attachment belongs to. Null when it is not tied to a message.
 */
messageId: string | null, 
/**
 * What kind of attachment this is.
 */
kind: MediaKind, 
/**
 * Which copy this is, as advertised by the message.
 */
variant: MediaVariant, 
/**
 * Which copy the cached bytes really are, once checked. Null until bytes are cached.
 */
actualVariant: MediaVariant | null, 
/**
 * `pending` (not downloaded yet), `available` (downloaded and checked), `expired` (WeChat no
 * longer has it) or `unsupported`.
 */
state: string, 
/**
 * MIME type of the cached bytes. Null until they are cached.
 */
contentType: string | null, 
/**
 * Size in bytes of the cached bytes that were checked. Null until they are cached.
 */
byteLength: number | null, 
/**
 * Size in bytes the sender advertised, which can differ from the checked size. Omitted when
 * the sender gave none, which does not mean the file is empty.
 */
advertisedByteLength?: number, 
/**
 * True when the account's current download setting allows this copy to be downloaded
 * automatically.
 */
autoDownloadAllowed?: boolean, 
/**
 * SHA-256 of the cached bytes, as lowercase hex. Null until they are cached.
 */
sha256: string | null, 
/**
 * File name of the attachment, when it has one. Null otherwise.
 */
filename: string | null, 
/**
 * When WeChat's copy expires, as Unix time in seconds. Null when WeChat gave no expiry.
 */
expiresAt: number | null, 
/**
 * Machine-readable code explaining the state, such as `media_expired`. Null when there is
 * nothing to explain.
 */
reason: string | null, 
/**
 * True when asking for the download again can still succeed.
 */
retryable: boolean, 
/**
 * ID of the job downloading or sending this attachment. Null when there is none.
 */
jobId: string | null, 
/**
 * Path on the daemon to fetch the cached bytes from. Null until the attachment is available.
 */
contentUrl: string | null, 
/**
 * Number of frames when the attachment is an animated image. Null otherwise.
 */
animationFrames: number | null, 
/**
 * Whether the downloaded copy was also saved into the official profile's own media folders:
 * `complete`, `pending`, `blocked`, or `unsupported` for media that is never saved there.
 */
officialWriteback: string, preview?: MediaImagePreview, };
export type NativeMediaBatchRequest = { 
/**
 * IDs of the messages to look up, from 1 to 40 per request.
 */
messageIds: Array<string>, };
export type NativeMediaBatchItem = { 
/**
 * ID of the message that was looked up.
 */
messageId: string, 
/**
 * Attachments of the message. Empty when `error` is set.
 */
assets: Array<MediaAsset>, 
/**
 * Error code when this message's attachments could not be listed, otherwise null.
 */
error: string | null, };
export type NativeMediaBatch = { 
/**
 * One entry for each requested message.
 */
items: Array<NativeMediaBatchItem>, };
export type MediaJob = { 
/**
 * Job ID.
 */
id: string, 
/**
 * ID of the account the job belongs to.
 */
accountId: string, 
/**
 * `send` for an upload to a chat, or `download` for fetching a received attachment.
 */
kind: string, 
/**
 * Where the job stands: `queued`, `running`, `available` (download finished), `accepted`
 * (WeChat took the send), `needs_review` (the outcome is unknown, so check before sending
 * again), `cancelled`, `expired`, `unsupported` or `failed`.
 */
state: string, 
/**
 * Current step: `queued`, `preparing`, `downloading`, `reserving`, `uploading`, `submitting`,
 * `readback` or `completed`.
 */
phase: string, 
/**
 * How many times the job has been started. A failed job can be retried only while this is
 * below 3.
 */
attempts: number, 
/**
 * True when a cancel has been requested for the job.
 */
cancelRequested: boolean, 
/**
 * True when the job can be retried.
 */
retryable: boolean, 
/**
 * Error code of the last failure. Null when there has been none.
 */
reason: string | null, 
/**
 * ID of the attachment being downloaded. Null for sends.
 */
assetId: string | null, 
/**
 * ID of the uploaded source being sent. Null for downloads.
 */
sourceId: string | null, 
/**
 * ID of the chat a send goes to. Null for downloads.
 */
threadId: string | null, 
/**
 * WeChat's ID of the sent message once WeChat accepted it. Null until then.
 */
serverMessageId: string | null, 
/**
 * Bytes transferred so far.
 */
completedBytes: number, 
/**
 * Total bytes to transfer, when known. Null otherwise.
 */
totalBytes: number | null, 
/**
 * Details of the outcome. Its fields depend on the job, and it is null until there is
 * something to report.
 */
result: unknown, 
/**
 * When the job was created, as an RFC 3339 time.
 */
createdAt: string, 
/**
 * When the job last changed, as an RFC 3339 time.
 */
updatedAt: string, };
export type OwnedMediaSource = { 
/**
 * Source ID. Pass it as `sourceId` when sending.
 */
id: string, 
/**
 * File name given at upload.
 */
name: string, 
/**
 * MIME type given at upload.
 */
mediaType: string, 
/**
 * Size of the file in bytes.
 */
byteLength: number, 
/**
 * SHA-256 of the file's bytes, as lowercase hex.
 */
sha256: string, 
/**
 * When the upload was stored, as an RFC 3339 time.
 */
createdAt: string, };
