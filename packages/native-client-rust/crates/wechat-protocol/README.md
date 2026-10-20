# wechat-protocol

Blocking native protocol library for the Linux 4.1.13.26 profile. This is Rust
protocol code using OpenSSL primitives; it does not execute Node or load an
official WeChat executable/library. Async callers must use a blocking worker.

The transport authenticates the configured public server pins, handshake
Finished values, every GCM record, business signatures (credential 1), and
business GCM envelopes before decoding service messages. No insecure peer or
signature-bypass setting exists. Routing redirects are accepted only from
authenticated responses and are restricted to supported official domain suffixes.

The crate includes QR login, QR polling/approval exchange, saved-device phone
approval, trusted AutoAuth renewal, NewSync cursor merging, heartbeat, text send,
known contact and group lookups, contact bootstrap pages, avatar bytes and voice
download chunks. The CDN module supports authenticated routing bootstrap, bounded
encrypted range downloads, small-object uploads, supplied JPEG image variants and
small MP4/thumbnail uploads. Metadata finalizers cover files, images, videos and
bounded preencoded SILK voice notes. These helpers do not include an audio/video
encoder, a generic image resizer or a full media job coordinator.

Results preserve unsigned 64-bit message IDs as decimal strings.
Unknown command payloads and contact fields remain available as original bytes.
All write/send methods submit once. A response failure after submission is an
uncertain outcome; the service owns durable intents and must not resend blindly.

`AccountState` and `LoginAttempt` are serializable **secret state**, with redacted
Debug implementations and zeroization on drop. Store them only in the encrypted
service vault. Do not return their serialized representation from public APIs.
`AccountState::from_baseline_json` converts an explicitly selected private
standalone checkpoint; it does not discover official profiles or read their files.

Tests include offline synthetic and known-answer checks. A separately authorized
isolated AMD64 validation also accepted one trusted AutoAuth renewal after an
authenticated session-expired response, a heartbeat and one NewSync page (125
commands, including 117 AddMsg records), using this Rust implementation with zero
QR requests, phone prompts or sends. Original account/checkpoint files were
unchanged. This is one test account/build result, not universal server acceptance.

The Rust QR/saved-device flows, contact/room operations and media network methods
have not received new live end-to-end validation in this implementation pass.
Their codecs follow previously verified native recipes. Group sends, group
creation, remote logout, sticker sends, active push association and a long-lived
longlink reconnect supervisor remain outside the verified scope. Avatar bytes
still need full bounded image decoding in the caller. QR images receive bounded
PNG-header checks only; supplied JPEG upload variants are fully decoded with
pixel/allocation limits. MP4 preparation validates container signature, not full
audio/video decoding. Voice send accepts an existing short format-4 SILK object;
it neither records nor encodes audio.

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
