# wechat-protocol

Blocking native protocol library for the Linux 4.1.13.26 profile. This is Rust
protocol code using OpenSSL primitives; it does not execute Node or load an
official WeChat executable/library. Async callers must use a blocking worker.

The transport authenticates the configured public server pins, handshake
Finished values, every GCM record, business signatures (credential 1), and
business GCM envelopes before decoding service messages. No insecure peer or
signature-bypass setting exists. Routing redirects are accepted only from
authenticated responses and are restricted to supported official domain suffixes.

The conditional LAN login challenge is experimental. Notification field17 is an
SKBuffer containing a non-store risk extension; state11 starts CGI22312 with
local IPv4 address/port entries, the current login identifier and a fresh
16-byte SKBuffer. A temporary TCP listener replies with a big-endian u32 total
length followed by JSON containing the returned opaque token and string
`scene: "xwechat"`. It never grants authorization; normal polling and final
authentication still apply. Listener/token state is never serialized and is
bounded by cancellation and the attempt deadline. Upload submission is attempted
once for an issued login identifier; an uncertain upload is not retried by the
repeatable polling path. `poll_login_guarded` lets an owner supply an earlier
deadline and nonblocking cancellation callback. This path has offline fixtures
and simulated-phone coverage; live phone/server acceptance remains unverified.

The crate includes QR login, QR polling/approval exchange, saved-device phone
approval, trusted AutoAuth renewal, NewSync cursor merging, heartbeat, text send,
known contact and group lookups, contact bootstrap pages, avatar bytes and voice
download chunks. The CDN module supports authenticated routing bootstrap, bounded
encrypted range downloads and uploads below the native 25 MiB big-file branch,
supplied JPEG image variants and MP4/thumbnail uploads. Metadata finalizers cover
files, images, videos and bounded SILK voice notes. The bundled portable SILK SDK
encodes fresh mono 16 kHz audio and decodes incoming voice bytes without loading
an official client codec. A separate core layer owns resizing, input conversion,
durable jobs, upload readback and uncertain-send handling. Sticker retrieval is
restricted to the verified original-byte CDN host, with pinned public DNS,
redirect refusal, MD5 validation and bounded full image decoding.

Results preserve unsigned 64-bit message IDs as decimal strings.
Unknown command payloads and contact fields remain available as original bytes.
All write/send methods submit once. A response failure after submission is an
uncertain outcome; the service owns durable intents and must not resend blindly.

`AccountState` and `LoginAttempt` are serializable **secret state**, with redacted
Debug implementations and zeroization on drop. Store them only in the encrypted
service vault. Do not return their serialized representation from public APIs.
The optional 32-byte official profile password and raw auth/account/network/AX
sections stay in that protected state. AuthSect field 9 changes the password only
when the native authentication update gate is set and the field is present.
`AccountState::from_baseline_json` converts an explicitly selected private
standalone checkpoint; it does not discover official profiles or read their files.

Tests include offline synthetic and known-answer checks. A separately authorized
isolated AMD64 validation also accepted one trusted AutoAuth renewal after an
authenticated session-expired response, a heartbeat and one NewSync page (125
commands, including 117 AddMsg records), using this Rust implementation with zero
QR requests, phone prompts or sends. Original account/checkpoint files were
unchanged. This is one test account/build result, not universal server acceptance.

The media network methods follow previously verified native recipes. Expanded
multi-range MP4 uploads still require isolated live validation. Group sends,
group creation, sticker sends, active push association and a long-lived longlink
reconnect supervisor remain outside the verified scope. Avatar bytes
still need full bounded image decoding in the caller. QR images receive bounded
PNG-header checks only; supplied JPEG upload variants are fully decoded with
pixel/allocation limits. MP4 preparation validates container signature, not full
audio/video decoding. Voice finalization accepts short format-4 SILK objects; the bundled encoder can
produce those from fresh PCM. It does not record a microphone.

NewSync continuation follows raw returned versus sent cursor bytes; committed
cursors use the unsigned per-key maximum merge (or special -2006 replacement).
`wire::decode_add_message` retains fields 6/7 as numbered values. Present field 13
is also exposed as `server_sequence` after 319 exact retained-message joins all
matched the independently saved official `server_seq` value. Missing field 13
remains `None`, not zero, and this correlation does not validate arbitrary message
classes for official database writes. A group's actual member sender remains
unresolved until separate envelope/member validation. The supported incoming
path is bounded heartbeat selector hints (`selector & 7`) followed by NewSync
scene 7; a transport-ready longlink alone does not prove account push association.

The HTTP carrier is port 80 with authenticated MMTLS data. System DNS resolution
is outside a strict end-to-end deadline; subsequent connect/read/write and HTTP
requests are bounded. A caller cancellation does not stop an in-progress blocking
send, so ownership and completion must remain with the service actor.

Run `cargo test -p wechat-protocol` from the native Rust workspace. See `NOTICE`
for the public fixture license and pin provenance.
