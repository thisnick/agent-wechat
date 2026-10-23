# Ordinary-file transfer validation

The initial native ordinary-file ceiling is 33,554,432 plaintext bytes (32 MiB).
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

## Bounds and recovery

Transfers still hold whole buffers. The source is released before readback, but
prepared ciphertext, downloaded ciphertext and plaintext can each occupy tens
of MiB. Concurrent account transfers need additional memory. The 32 MiB ceiling
stays below the existing 128 MiB profile-cache, stored-content, SDK-content and
OpenClaw facade bounds; those bounds and image decoder limits are unchanged.
The encrypted source vault admits exactly 32 MiB plus its existing envelope.

Upload material, reservation receipts and completed object descriptors are
stored with the job. Recovery after completed upload can repeat owned-object
readback. Uncertain reservation, upload and final submission states require
review; recovery never automatically resends a possibly accepted message.
Cancellation, logout and account-generation checks remain between exchanges.

Server cache hits, requested retries, alternate routing and unrecognized
response controls stop the transfer. Existing-ciphertext key recovery is not
implemented. The validated path is a fresh cache miss, with bounded advertised
512 KiB or 1 MiB blocks and checked acknowledgements, offsets and final metadata.
No group, forwarding, large image/video/voice, live ARM64 or arbitrary official
build compatibility is implied.

## Reproduction and checks

With an isolated account and a confirmed destination:

```sh
wx account capabilities
wx messages send filehelper --file ./synthetic-32MiB.pdf \
  --idempotency-key UNIQUE_KEY --wait 300
wx jobs get JOB_ID
wx messages get MESSAGE_ID
wx messages media filehelper MESSAGE_ID --output ./saved.pdf --cached-only
sha256sum ./synthetic-32MiB.pdf ./saved.pdf
```

Keep the original key and receipt after a timeout. Inspect the same job instead
of creating another intent. Check the recipient's download/opening separately.

Validation gates include the pinned 1.93.1 formatter and warning-denying Clippy,
the full Rust workspace tests, generated-contract check, workspace build and
type checks, OpenClaw tests and the repository's AMD64 image build. Synthetic
tests cover padding boundaries, corrupt and incomplete replies, cancellation,
reservation/restart uncertainty, idempotency and per-kind admission limits.
