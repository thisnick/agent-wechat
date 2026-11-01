# wechat-protocol

Blocking native protocol library for the Linux 4.1.13.26 profile. This is Rust
protocol code using OpenSSL primitives; it does not execute Node or load an
official WeChat executable/library. Async callers must use a blocking worker.

## Start with NativeClient

Most server operations are methods on
[`NativeClient`](https://docs.rs/wechat-protocol/latest/wechat_protocol/struct.NativeClient.html).
The crate-level Functions list mainly contains helper functions.

| Task | API |
| --- | --- |
| Login | `start_qr_login`, `poll_login`, `complete_login` |
| Receive | `sync`, `sync_scene`, `heartbeat`, `push_connection` |
| Send | `send_text`, `send_share`, `send_quote`, `send_file`, `send_image`, `send_video`, `send_voice` |
| Contacts | `lookup_contacts`, `lookup_room`, `init_contacts` |
| Lower-level operations | `wire`, `media`, group request builders and response decoders |

`AccountState` holds the accepted credentials and is passed mutably because
responses can update credentials and routing. The daemon's native upstream
adapter calls these APIs; it adds account coordination, recovery and profile
writeback above them. Some operations use a request builder, a client RPC method
and a decoder instead of one convenience method.

From a repository checkout, run one of these examples:

```bash
cargo run --manifest-path packages/native-client-rust/Cargo.toml -p wechat-protocol --example qr_login
cargo run --manifest-path packages/native-client-rust/Cargo.toml -p wechat-protocol --example send_text -- RECIPIENT UNIQUE_TASK_ID "Hello from the Rust example"
cargo run --manifest-path packages/native-client-rust/Cargo.toml -p wechat-protocol --example sync_once
```

Each requests a fresh QR login for a test account, prints a private temporary
PNG path to open and waits for phone approval. The QR is removed after login;
credentials stay in memory. `send_text` submits once and prints the receipt;
`sync_once` summarizes one page without saving history or a cursor. These are
standalone examples, not consumers of an existing daemon session. A fresh login
may replace another active desktop login. `--help` makes no network requests.

The sender requires an exact username rather than a contact's display name.
A task ID is not a server idempotency guarantee: investigate an uncertain send
before running it again. Production applications must save the operation and
its timestamp before submitting and keep secret account state encrypted.

See the [Rust libraries guide](https://thisnick.github.io/agent-wechat/reference/rust-libraries/#send-one-text-message)
for a short text-send example using an already authenticated account.

## Transport and supported operations

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
download chunks. The CDN module implements authenticated routing bootstrap,
encrypted range downloads, ordinary-file uploads with an initial 32 MiB plaintext
ceiling, supplied JPEG image variants and MP4/thumbnail uploads. Metadata finalizers cover
files, images, videos and bounded SILK voice notes. The bundled portable SILK SDK
encodes fresh mono 16 kHz audio and decodes incoming voice bytes without loading
an official client codec. A separate core layer owns resizing, input conversion,
durable jobs, upload readback and uncertain-send handling. Sticker retrieval is
restricted to the verified original-byte CDN host, with pinned public DNS,
redirect refusal, MD5 validation and bounded full image decoding.

The native ordinary-file implementation selects the four-phase
apply/uploadblock/fin/complete carrier when PKCS#7 ciphertext reaches 25 MiB.
This uses the reference's supported multiphase policy for the 16-byte padding
band below 25 MiB plaintext; the reference's default binary-stream `uploadbig`
carrier is not implemented. Plaintext above 25 MiB selects filetype 7 and final
app-message field 6=7. Upload and download independently select the authenticated
safe C2C routing table with their respective operation selectors; upload routing
type 7 is separate from the request's actual filetype 5 or 7. Filetype7 readback uses
`downloadbig`, exact encrypted and plaintext sizes, source MD5 and caller-owned
SHA-256 verification before final submission. Ordinary image/video/voice upload
limits remain 26,214,383 bytes.

`NativeClient::reserve_file_upload` submits CGI6691 once using a separate,
caller-persisted progress ID. Its `FileUploadReservation` contains secret token
material with redacted Debug and zeroization; protect it with the upload key,
source and final client ID. `send_file_with_reservation` emits the reservation
metadata and correlates the final CGI222 receipt. A response failure must not
cause a new reservation or final send. Existing no-reservation APIs retain their
previous wire form. Large-file cache hits and server retry/routing controls are
rejected rather than guessed. The path has synthetic schema/transfer tests and
an isolated native 32 MiB File Transfer PDF accepted with exact readback,
recipient opening and complete local writeback. Cold CLI/SDK byte and range
checks passed; recipient-exported SHA-256 remains unavailable.
See [FEATURES.md](../../FEATURES.md) for the current validation record.

## Experimental call codecs

The opt-in Rust feature `experimental-calls` exposes offline signaling and
incoming notification envelopes, personal Invite/Cancel/LiveSync codecs, selected
app-session negotiation and relay packet codecs under `calls`. These prepare and
validate owned bytes with account-generation bindings. The feature does
not enable voice or video calls.
There is no call transport, codec negotiation, microphone/camera access or
`NativeClient` call method. Keys, media framing and session lifetimes must be
verified before connecting these building blocks to a live transport. Existing
voice-note and video-file methods remain attachment operations.

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
audio/video decoding. Voice finalization accepts format-4 SILK notes of up to 60
seconds and 186,010 encoded bytes, a bound derived from the bundled 16 kbit/s,
20 ms profile with 1.5x rate headroom; the bundled encoder can produce those from
fresh PCM. A short-link request travels in one MMTLS record, so a note above
32 KiB is submitted as consecutive `uploadvoice` offset/length ranges of one client
message ID, with the end flag on the last range and the server message ID from an
earlier range carried forward. That multi-range path has offline fixtures only and
still requires live validation. It does not record a microphone.

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
