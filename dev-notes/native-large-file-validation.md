# Ordinary-file transfer validation

The native ordinary-file ceiling is 536,870,912 plaintext bytes (512 MiB). The first validation used a 32 MiB ceiling; the larger-path results are recorded separately below.
It is a product limit, not a statement of WeChat's maximum file size. Image,
video and voice source limits remain 26,214,383 bytes. Sticker sending is not
implemented.

## Reference and scope

Reference observations used the Linux x64 WeChat 4.1.13.26 executable, SHA-256
`2749535f299ba67feb8ef80cab898bb0531fa8f376b16f8a72db706811c84132`.
Tests used synthetic files, an isolated account and the operator-approved File
Transfer destination. Native execution uses no official executable. Raw
captures, credentials, routing values and profiles are retained privately.

Static analysis and observed execution establish different boundaries:

| Condition | Selection |
| --- | --- |
| PKCS#7 ciphertext reaches 25 MiB | Reference task labels the upload `uploadbig` and selects safe routing type 7 |
| Plaintext reaches 25 MiB | Reference default selects apply, uploadblock, fin and complete |
| Plaintext exceeds 25 MiB | File type 7 and final app-message outer field 6 = 7 |

The reference's 16-byte padding band below 25 MiB uses a binary stream carrier;
a flat HTTP `/uploadbig` request was rejected before storage. The native client
selects the reference's supported multiphase policy whenever ciphertext reaches
25 MiB, including that band. This is a deterministic selection, not a fallback
or retry after a failed transfer. The legacy HTTP uploader rejects this band
before network I/O.

The observed multiphase cache-miss sequence does not execute `checkmd5big`.
Authentication and routing come from the authenticated CDN bootstrap's safe
routing table. Upload and download select their own routing operation; routing
type 7 is independent of the request's file type 5 or 7. A file-message
reservation precedes upload; its token and message
reference are saved before proceeding. Completion provides an object descriptor,
which must pass full plaintext readback and SHA-256 verification before final
message submission.

## Results recorded on 2026-10-22

Storage, message acceptance, recipient opening and local profile writeback are
separate checks. A successful object-only test creates no chat message.

| Test | Result |
| --- | --- |
| Official 1 MiB control | File Transfer message observed |
| Official 32 MiB reference | Business response accepted; recipient confirmed download completed; exported recipient hash unavailable |
| Independent 32 MiB object upload/readback | Fresh object completion and exact plaintext SHA-256 matched; no message created |
| Native Rust 32 MiB object upload/readback | Exact 33,554,432-byte readback and saved-byte equality; no message created |
| Native Rust exact 25 MiB | Multiphase type 5 upload; both safe and normal `/download` readbacks matched the same object |
| Native Rust 25 MiB + 1 byte | Multiphase type 7 upload and exact `/downloadbig` readback matched |
| Native Rust 25 MiB - 17 bytes | Existing `/uploadv3` control and exact `/download` readback matched |
| Native Rust 25 MiB - 16 bytes | Forced multiphase type 5 upload, normal `/download` readback and saved-byte equality passed |
| Native Rust 25 MiB - 1 byte | Forced multiphase type 5 upload, normal `/download` readback and saved-byte equality passed |
| Final download routing | Read back the already-owned 32 MiB object using the separate typed download selector; exact source and saved-byte equality; no upload or message |
| Built native daemon 32 MiB PDF | One accepted attempt; exact pre-send SHA-256 readback; recipient downloaded/opened and confirmed the visible code |
| Native cold restart | CLI and SDK full bytes/SHA-256 matched; prefix, suffix and end-of-file HTTP ranges returned correct 206 responses; same-key lookup retained one accepted job |
| Database and file cache | Supported official-profile history row type 49/subtype 6; profile and media writeback complete; stored source and cache hashes/sizes matched |

The native Rust 32 MiB object test took 84.9 seconds to upload and 30.4 seconds
to read back. Peak RSS was 144,980 KiB. Exact 25 MiB type 5 readback used 64 KiB
ranges and took about 111-116 seconds per readback. These are single-host
observations, not latency or memory guarantees.

Exact padding coverage used these plaintext/ciphertext byte pairs:
26,214,383/26,214,384; 26,214,384/26,214,400;
26,214,399/26,214,400; 26,214,400/26,214,416;
26,214,401/26,214,416; and 33,554,432/33,554,448.
The two forced-multiphase padding cases took 178.8 and 180.8 seconds respectively,
including normal File-type readback. The final separate large-download route
read back the retained 32 MiB object in 30.6 seconds.

The built-daemon PDF send completed in 122.66 seconds. The largest native
process high-water RSS was 157,280 KiB; the maximum one-second sample of combined
daemon/CLI RSS was 138,568 KiB and can miss short peaks. The retained source and
cached asset each occupy 33,554,461 encrypted bytes in the sidecar, in addition
to the official-profile cache, SQLite pages and transient WAL space. The source
SHA-256 is `d13a09b07275ae1bacd0ae79c23728a8d1811324d2ebb228a75c8960f8e4253c`.
The recipient could not export a saved file for hashing; phone byte equality is
unverified. A separate official-client session displayed the native PDF message.
That is not a profile-handoff or FTS-compatibility test.

## 50 MiB control

After the 512 MiB readback failures below, the operator approved a smaller
52,428,800-byte synthetic PDF. The built daemon completed upload, full pre-send
readback and one accepted business-message submission in 200.68 seconds. The
source SHA-256 was
`04ed369c0e8ab2d92255dc73d7f6b226d311b94c2260afd7e13c694e3a068adc`.

| Check | Result |
| --- | --- |
| Message acceptance | One worker attempt, matching client ID and successful server receipt |
| Cold CLI and SDK retrieval | Both saved all 52,428,800 bytes with the exact source SHA-256; same job and server receipt |
| HTTP ranges | Prefix, suffix and end-clipped ranges returned correct 206 bytes and ETags |
| SDK cancellation and bounds | Cancellation before/after the first chunk, single-use streams and an undersized response bound behaved as expected |
| OpenClaw storage | Real pinned runtime saved the API bytes in isolated storage; size/SHA-256 and the file-reference manifest matched; no model or Gateway dispatch |
| Official-profile and sidecar writes | One type 49/subtype 6 history row; source/asset authenticated decryption, XML object/key/reservation correlation, resource/hardlink rows and saved cache size/MD5/SHA-256 all matched |
| Database preservation | Five original database-family members retained their hashes and timestamps during verification on copies |
| Recipient opening | Account owner downloaded/opened the PDF on the phone and confirmed its visible code; exported phone SHA-256 unavailable |

The isolated daemon returned online after cold restart without another login.
Read-only post-send checks created no upload or business message. The maximum
one-second combined daemon/CLI RSS sample during the send was 154,948 KiB;
shorter peaks can be missed. The daemon's process high-water mark included the
earlier 512 MiB attempt and is not a 50 MiB memory measurement. This control
does not resolve or validate the separate 512 MiB final-message failures.

## Extension through 512 MiB

| Milestone | Result |
| --- | --- |
| Independent fresh object | All 513 negotiated 1 MiB blocks, final 16-byte padding block and 516 requests completed in 1,401.56 seconds; no business message |
| Actual Rust readback | Exact 536,870,912 bytes, full source/saved-byte equality and SHA-256 matched in 569.92 seconds; process peak RSS 1,060,332 KiB |
| Rolling acknowledgements | Service accepted 16-, 32- and 48-byte windows, including boundaries at blocks 128, 256, 384 and 512 |
| Real OpenClaw storage | Pinned Node 24.21.0/OpenClaw 2026.10.2 streamed all 512 MiB, fsynced and independently re-read the saved file with exact SHA-256; process peak RSS 249,148 KiB; no API or send |
| Account sync during native upload/readback | Succeeded in 1.58 seconds during upload and 1.55 seconds during readback while the same file job remained running with one attempt; no extra message |
| Built native file message | First worker attempt completed upload, then stopped at 357,040,128 readback bytes with `unsupported_media`; no final business-message submission. The exact rejected response was not retained |
| Retained PDF object | A separate full read-only capture checked all 1,025 ranges and matched every source byte and SHA-256 in 524.49 seconds; no upload or message |
| Explicit completed-upload reuse | The same job's second worker attempt stopped at 342,360,064 readback bytes with `media_processing_failed`; no new reservation/upload or final message submission |
| Recipient opening, cold API/CLI/SDK and database/cache checks | Pending investigation of the failed final-client readback |

The fixed 128-block limit belonged to the initial native implementation. The
reference uses a rolling window of the current 128-block group and its two
preceding groups. File-relative offsets and all ordered block etags remain
unchanged. The native request-field ceiling admits 1,025 blocks at a negotiated
512 KiB size while retaining the independent 2 MiB request-byte envelope.
Unknown block sizes and controls still stop the transfer.

The 512 MiB initial maximum is a product limit, not an established service
maximum. The original 25 MiB padding/type/routing boundaries still apply.
Image/video/voice, group, forwarding and live ARM64 claims are unchanged.

## Bounds and recovery

Transfers still hold whole buffers. The source is released before readback;
prepared ciphertext, downloaded ciphertext and plaintext can each occupy
512 MiB. Prepared validation decrypts into a reusable 128 KiB buffer. Download
reserves the exact padded size rather than doubling capacity at the boundary.
SQLite retains whole encrypted BLOBs, and updates can use additional internal
buffers. Concurrent accounts and source uploads need additional memory.

Ordinary-file source, cache, profile and retrieval bounds now admit 512 MiB;
image/video/voice source limits and nonfile content bounds are unchanged. The
vault retains its existing format and account/purpose binding: only media
source/asset records get 512 MiB, plus the 29-byte envelope. Generic control
records retain 32 MiB. Metadata reads do not load attachment BLOBs. File
writeback borrows owned bytes, and HTTP ranges share the verified allocation.
CLI retrieval and OpenClaw storage stream verified bytes into private files;
OpenClaw does not base64-encode large attachments.

One media job per account remains active. Ordinary-file CDN I/O releases the
account mutation guard, so push heartbeats and sync can proceed. The worker
reacquires that guard before business RPCs or profile changes, verifies the
accepted authorization and logout generation, checks cancellation and reloads
the current cookie. A correlated upload completion is saved before late fence
checks; it is never discarded merely because cancellation arrived in flight.

Upload material, reservation receipts and completed object descriptors are
stored with the job. Recovery after completed upload can repeat owned-object
readback. Uncertain reservation, upload and final submission states require
review; recovery never automatically resends a possibly accepted message.
Cancellation, logout and account-generation checks remain between exchanges.

Non-success service responses, server cache hits, unclassified routing/format
controls and unknown response fields stop the transfer. Existing-ciphertext key recovery is not
implemented. The validated path is a fresh cache miss, with bounded advertised
512 KiB or 1 MiB blocks and checked acknowledgements, offsets and final metadata.
No group, forwarding, large image/video/voice, live ARM64 or arbitrary official
build compatibility is implied.

The matching Linux file7 success handler does not consume eight named response
metadata fields: `retrysec`, `isretry`, `isoverload`, `isgetcdn`, `istrans`,
`backsrcnum`, `substituteftype` and `rsppicformat`. The native client accepts these
optionally as unsigned decimal values within its 16-byte parser bound. They do
not trigger a retry or change the requested file representation. Version and
video-format values, unknown fields and all integrity checks remain strict.
This static classification does not identify the original unrecorded failure.
New failures report only a known numeric control or a generic unknown-field
classification, excluding response payloads, keys and object identifiers.
Other readback failures can still collapse to `media_processing_failed` without
a retained cause. The later second-attempt range served successfully in a
separate read-only probe; this does not identify the failed exchange or establish
a size boundary. Both failed attempts remain recorded, with no final message
receipt, and the original completed object is preserved.

An operator can request `--reuse-completed-upload` on an unsupported file job
that stopped in readback before final submission. Its separate API endpoint
prevents older servers from interpreting this as an ordinary retry. Admission
atomically binds the unchanged job, source, complete request/reservation/upload
material and current accepted authorization. The worker takes a separate path
that cannot reserve or upload, verifies the complete source and readback again,
and only then journals its first final submission. Cancelled, changed, malformed
or uncertain submission state remains blocked. Worker attempts and actual
business-message submissions must be counted separately.

## Reproduction and checks

With an isolated account and a confirmed destination:

```sh
wx account capabilities
wx messages send filehelper --file ./synthetic-512MiB.pdf \
  --idempotency-key UNIQUE_KEY --wait 3600
wx jobs get JOB_ID
wx messages get MESSAGE_ID
wx messages media filehelper MESSAGE_ID --output ./saved.pdf --cached-only
sha256sum ./synthetic-512MiB.pdf ./saved.pdf
```

Keep the original key and receipt after a timeout. Inspect the same job instead
of creating another intent. Check the recipient's download/opening separately.

Validation gates include the pinned 1.93.1 formatter and warning-denying Clippy,
the full Rust workspace tests, generated-contract check, workspace build and
type checks, OpenClaw tests and the repository's AMD64 image build. Synthetic
tests cover padding boundaries, corrupt and incomplete replies, cancellation,
reservation/restart uncertainty, idempotency and per-kind admission limits. A paused-transfer fixture also verifies that account work can proceed while a second media job stays serialized; cookie updates resume safely, and cancellation, logout, credential rotation, epoch changes or disconnect stop the next submission while retaining completed upload receipts. OpenClaw fixtures retain the same incoming download job after temporary status GET failures across a ten-minute wait and `media.ready`, then dispatch once. Prepared attachment manifests admit exactly 512 MiB ordinary files and reject one byte above, while nonfile bounds remain unchanged.
