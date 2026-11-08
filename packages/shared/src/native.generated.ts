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
export type NativeMentions = { state: string, userIds: Array<string>, self: boolean | null, all: boolean | null, source: string | null, };
export type NativeQuoteReference = { serverId: string, type: number, state?: string, senderId?: string, direction?: string, appMessageType?: number, embeddedText?: string, embeddedImage?: boolean, embeddedLink?: LinkCard, embeddedSticker?: boolean, };
export type LinkCard = { title: string | null, description: string | null, url: string | null, sourceName: string | null, };
export type LocationCoordinateSystem = "unknown";
export type LocationCard = { latitude: number, longitude: number, name: string | null, address: string | null, scale: number | null, mapType: string | null, poiId: string | null, coordinateSystem: LocationCoordinateSystem, };
export type NativeRecallInfo = { observedAt: string, recalledAt?: string, replacementText?: string | null, };
export type NativeMessage = { id: string, accountId: string, threadId: string, serverId: string | null, senderId: string | null, type: number, appMessageType?: number, quote?: NativeQuoteReference, link?: LinkCard, location?: LocationCard, recall?: NativeRecallInfo, direction: string | null, body: string | null, createdAt: string | null, mentions: NativeMentions, version: string, media: unknown[], };
export type PeerCategory = "unknown" | "system" | "group" | "business" | "openim" | "openim_customer_service";
export type PeerNamespace = "wechat" | "openim" | "openim_customer_service" | "im_chatroom" | "qy" | "weclaw" | "other";
export type PeerBusinessKind = "official_account" | "service_account" | "enterprise" | "enterprise_child" | "photo_account";
export type PeerWording = { languageId: number, text: string, };
export type PeerApplication = { languageId: number, accountTypeId: string | null, accountTypeCached: boolean, };
export type PeerOpenIm = { appId: string | null, wordingId: string | null, wordings: Array<PeerWording>, applications: Array<PeerApplication>, };
export type PeerClassification = { category: PeerCategory, namespace: PeerNamespace, businessType: number | null, businessKind: PeerBusinessKind | null, openIm: PeerOpenIm | null, };
export type NativeThread = { id: string, accountId: string, peerId: string, kind: string, classification?: PeerClassification, displayName: string | null, unreadCount: number | null, muted?: boolean | null, pinned?: boolean | null, readStateVersion: string, lastMessage?: NativeThreadMessage | null, avatarMembers?: Array<NativeThreadAvatarMember>, history: unknown, };
export type NativeThreadMessage = { id: string, senderId: string | null, senderName: string | null, direction: string | null, type: number, text: string, createdAt: string | null, };
export type NativeThreadAvatarMember = { id: string, displayName: string, };
export type NativeContact = { id: string, username: string, displayName: string | null, nickname: string | null, remark: string | null, alias: string | null, relation: string, classification?: PeerClassification, };
export type NativeGroup = { id: string, roomId: string, classification?: PeerClassification, displayName: string | null, ownerId: string | null, administratorIds?: Array<string> | null, canMentionAll?: boolean | null, announcement: string | null, name?: string | null, remark?: string | null, muted?: boolean | null, minimized?: boolean | null, isInGroup?: boolean | null, myDisplayName?: string | null, };
export type NativeGroupMember = { id: string, username: string, classification?: PeerClassification, displayName: string | null, roomDisplayName: string | null, mentionName?: string, };
export type NativeGroupMemberLookup = { groupId: string, username: string, state: string, member: NativeGroupMember | null, coverage: string, };
export type NativeGroupCreateRequest = { members: Array<string>, name?: string, };
export type NativeGroupNameRequest = { name: string, };
export type NativeGroupMembersRequest = { members: Array<string>, };
export type NativeGroupMyNameRequest = { displayName: string, };
export type NativeThreadNotificationsRequest = { muted: boolean, };
export type NativeThreadPinRequest = { pinned: boolean, };
export type NativeTombstone = { id: string, deleted: true, version: string, };
export type NativePageItem<T> = T | NativeTombstone;
export type NativePage<T> = { items: Array<NativePageItem<T>>, nextCursor: string | null, coverage: unknown, sourceGeneration: string, asOfEventCursor?: string, };
export type NativeOperation = { id: string, accountId: string, kind: string, state: string, result: unknown, createdAt: string, updatedAt: string, };
export type NativeSendIntent = { id: string, accountId: string, destination: string, state: string, clientId: string | null, serverId: string | null, errorCode: string | null, localMessageId?: string, createdAt: string, };
export type NativeEvent = { cursor: string, kind: string, data: Record<string, unknown>, createdAt: string, };
export type NativeIdentity = { product: string, backend: string, version: string, apiVersion: number, target?: string, capabilities: Array<string>, };
export type NativeStatus = { serverId: string, dataDir: string, pid: number, ownershipGeneration: string, version: NativeIdentity, state: string, };
export type AccountSettings = { autoDownloadMaxBytes: number, };
export type NativeAccountRemovalRequest = { confirmAccountId: string, };
export type NativeAccountRemoval = { accountId: string, removed: boolean, filesRetained: boolean, };
export type QuoteSendReference = { messageId: string, serverId: string, };
export type TextMentionPosition = { start: number, end: number, index: number, };
export type TextMentions = { userIds?: Array<string>, all?: boolean, positions?: Array<TextMentionPosition>, };
export type NativeTextSendRequest = { threadId: string, text: string, quote?: QuoteSendReference, mentions?: TextMentions, };
export type ShareSendRequest = { "kind": "share", threadId: string, title: string, url: string, description?: string, thumbnailUrl?: string, };
export type NativeRecallRequest = { serverId: string, };
export type NativeReadRequest = { expectedVersion: string, };
export type NativeForgetRequest = { confirm: boolean, };
export type NativeMediaWritebackRequest = { limit: number, };
export type NativeMediaJobRetryRequest = { reuseCompletedUpload?: boolean, };
export type OfficialProfileRequest = { path: string | null, accountId: string | null, };
export type OfficialProfileSelection = { accountId: string, profilePath: string, storageMode: string, savedDeviceLoginAvailable: boolean, };
export type LoginStrategy = "auto" | "saved-device" | "qr";
export type QrRefreshMode = "auto" | "manual";
export type QrRefresh = { mode: QrRefreshMode, maxRefreshes: number, };
export type LoginRequest = { strategy: LoginStrategy, qrRefresh: QrRefresh, allowQr: boolean, forceQr: boolean, timeoutSeconds: number, computerName: string, };
export type MediaKind = "image" | "file" | "video" | "voice" | "sticker";
export type MediaVariant = "best" | "thumbnail" | "standard" | "original" | "normal" | "encoded";
export type VideoSendQuality = "normal" | "original";
export type MediaTransferRequest = { messageId: string, assetId: string | null, quality: MediaVariant, };
export type MediaSendRequest = { threadId: string, kind: MediaKind, sourceId: string, standardSourceId: string | null, thumbnailSourceId: string | null, playSeconds: number | null, durationMs: number | null, videoQuality?: VideoSendQuality, };
export type MediaImagePreview = { sourceSha256: string, contentType: string, byteLength: number, sha256: string, filename: string, contentUrl: string, };
export type MediaAsset = { id: string, accountId: string, messageId: string | null, kind: MediaKind, variant: MediaVariant, actualVariant: MediaVariant | null, state: string, contentType: string | null, byteLength: number | null, advertisedByteLength?: number, autoDownloadAllowed?: boolean, sha256: string | null, filename: string | null, expiresAt: number | null, reason: string | null, retryable: boolean, jobId: string | null, contentUrl: string | null, animationFrames: number | null, officialWriteback: string, preview?: MediaImagePreview, };
export type NativeMediaBatchRequest = { messageIds: Array<string>, };
export type NativeMediaBatchItem = { messageId: string, assets: Array<MediaAsset>, error: string | null, };
export type NativeMediaBatch = { items: Array<NativeMediaBatchItem>, };
export type MediaJob = { id: string, accountId: string, kind: string, state: string, phase: string, attempts: number, cancelRequested: boolean, retryable: boolean, reason: string | null, assetId: string | null, sourceId: string | null, threadId: string | null, serverMessageId: string | null, completedBytes: number, totalBytes: number | null, result: unknown, createdAt: string, updatedAt: string, };
export type OwnedMediaSource = { id: string, name: string, mediaType: string, byteLength: number, sha256: string, createdAt: string, };
