# Native normal/original video validation

The implementation prepares normal playback before upload and can include the
unchanged source as a separate original video. Its initial source ceiling is
50 MiB (52,428,800 bytes), independently of the 512 MiB ordinary-file ceiling.
Image and voice limits are unchanged.

## Policy and interfaces

| Choice | Prepared objects | Final message |
| --- | --- | --- |
| Normal, default for new jobs | Compressed MP4 and JPEG thumbnail | Video with normal/thumbnail descriptors |
| Original | Same playback copy/thumbnail plus unchanged source | Video with separate raw descriptor, checksum, key and length |
| File kind / OpenClaw forceDocument | Unchanged source | Ordinary file attachment |

The normal policy is H.264 Main/yuv420p, longest edge at most 720 pixels,
30 frames per second, and stereo 48 kHz AAC at 64 kbps when audio is present.
Video bitrate is at most 2 Mbps and decreases to fit a conservative size budget.
Complete source/output frame counts and durations are checked; clipping at the
size guard is refused. This is our encoding policy, not a claim to duplicate the
official app's preset. Sources need one video stream and at most one audio
stream; preparation has a five-minute deadline and cancellation kills/reaps the
process.

Normal output remains within the existing paired video/thumbnail uploader's
26,214,383-byte limit. An original whose padded ciphertext reaches 25 MiB uses
a single type4 multiphase object. Large video readback retains type4 and uses
the video download route. Every object must match its saved SHA-256 before the
first business-message submission.

The CLI adds `--video-quality normal|original` with `--video`; API/SDK requests
add optional `videoQuality`. OpenClaw accepts
`channelData: { "agent-wechat": { "videoQuality": "original" } }` on the regular
message action. The pinned host's prepared, forced-core and persisted-queue
paths retain the option. `forceDocument` and its `asDocument` alias select an
ordinary file, and conflict with video quality. Selection/size checks precede
staging. Retrieval quality remains a separate option.

## Recovery and verification

Prepared normal bytes and thumbnails are saved before network upload. Normal
and original objects use separate saved encryption/upload material. Completed
object receipts are saved before late cancellation or authorization checks.
Long preparation/transfers release the account mutation guard while preserving
the media job lock and accepted-authorization/logout fences. Legacy sealed
plans without a quality field retain their prior byte-preserving behavior.
Source hashes and the quality choice are bound to idempotency; the CLI also
checks quality before returning an existing local receipt. A possibly accepted
message is never automatically resubmitted.

Local tests pass with Rust 1.93.1: pinned formatter/check, warning-denying
workspace/all-target Clippy, all 495 Rust tests and generated-contract checks.
SDK tests (37), extension tests (234), build and type checks pass. The pinned
OpenClaw 2026.10.2 test executes its actual message builder and queue against a
provider stub in isolated storage, with zero service calls.

## Live evidence

The built isolated Linux x64 daemon completed the normal-mode control on
2026-10-22 at about 21:24 Pacific, in one worker attempt. A 52,428,800-byte MP4
source became a 1,531,327-byte playback object plus a 7,095-byte thumbnail.
Both passed pre-submit readback; WeChat accepted one video message and local
history/media writeback completed. SDK full hashes and final HTTP 206 ranges
match both stored objects. The account owner confirmed playback with audio and
the visible verification code. Copied-database verification independently checked one type43 history row,
normal/thumbnail XML, encrypted owned sources/assets and official resources,
hardlinks and cache bytes. All five original database-family members and five
other source/key/cache files remained unchanged. The daemon restarted online
without login and the built CLI retrieved the cached normal video with the
expected hash. The built CLI rejects a different quality with
the same local receipt/key without changing that receipt.

Original mode completed its 50 MiB CDN upload but stopped in readback before
business-message submission. One retained read-only range returned the exact
source bytes, type4, correct range/size/checksum metadata, `istrans=999` and
`videoformat=1`. The initial conservative video validator rejected nonzero
controls. A source-bound review of the official normal pipeline established that the
selected success handler filters `istrans` and does not consume the literal
wire `videoformat` value. The native validator now admits only the observed
values alongside zero; unknown fields and other routing/format controls remain
strict. The complete retained object passed an independent 101-range readback
with exact source-byte equality. The stopped job and upload objects are
retained, with no automatic retry or message send.
A fresh control will validate the corrected daemon path; no original-mode recipient claim follows yet.

The controlled source contains short video/audio and a valid ISO-BMFF padding
atom to exactly 50 MiB. Ordinary-file success does not establish large-video
support. The daemon remained authenticated and online during the transfer.
