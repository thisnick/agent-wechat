# Native client implementation checklist

This checklist tracks implementation and validation separately. A command or
route appearing in help is not evidence of a successful native exchange. The
supported-profile runtime has explicit compatibility limits. The initial scope is taking over an existing
stopped official profile, using its saved identity and continuing supported
database/media writes in place. Fresh-profile creation, export and returning the
store to the official app are outside this scope.
No deployment or package publication is part of this implementation task.

Legend: **Implemented** means executable code and focused tests exist;
**Partial** identifies an unfinished required behavior; **Pending** identifies
work still required. Live validation is stated separately and never inferred
from fixture tests. Capability responses must reflect these limits.

| Required initial capability | Implementation | Validation / remaining work |
| --- | --- | --- |
| Independent protocol, profile, store, core and optional HTTP crates | Implemented | Library consumers and offline synthetic fixtures |
| Versioned configuration, XDG paths, protected registry/sidecars | Implemented | Precedence and unsupported-version tests; damaged database, WAL/SHM, rollback recovery and failed-migration fixtures preserve existing families |
| Separate server configuration and endpoint-scoped client defaults | Implemented | Actual CLI scoped endpoint/default smoke and ambiguity fixtures |
| Canonical daemon account aliases | Implemented | Actual daemon creation, rename, default selection and ambiguous selection checks |
| Simultaneous independent account actors | Implemented | Synthetic isolation passes. Only one isolated test account is available; live multi-account validation is explicitly unverified and deferred by the operator |
| Exclusive process/account/profile ownership | Implemented | Canonical database-root overlap regression; unmanaged official client must still be stopped explicitly |
| Foreground `serve`, background start/status/stop/logs | Implemented | Actual ELF background start/status/stop/restart with PID/store generation and occupied-owner checks |
| Protected active/saved credentials and independent API token rotation | Implemented | Restart, permission, redaction and cryptographic integrity tests; regional routing is journaled with saved credentials, with interruption/replay, source-conflict and unknown-setting preservation coverage. Full MMKV files grow by bounded pages without dropping unknown records; the live 4096→8192-byte credential recovery passed |
| Active authorization validation, bounded renewal and saved-device recovery | Implemented | Verified native exchanges are separate from universal lifetime/expiry claims |
| Single-switch in-place official-file login | Implemented | Account selection, full stored-device/prefix handling, restored key-info, distinct key/open-error admission, file-preservation and scoped-copy fixtures. Controlled original-profile QR approval with restored key-info unlocked all 20 existing encrypted databases; API/CLI history reads and a cold restart passed. All 1,438 original source files stayed unchanged. A fresh credential store then restored trust, key-info and saved regional routing from these files and logged in without a QR or additional phone approval; all 20 databases and history remained readable. A newer recorded official saved grant also beat a stale sidecar without QR; continued sync produced 43 File Transfer rows. All four media kinds downloaded with complete in-place writeback, cold hashes and Range206 readback |
| Explicit QR login, white PNG, scan/approval, cancellation, bounded refresh | Implemented | Pixel-grid white renderer test; actual CLI generation/PNG/interrupt fixture; controlled native account validation remains distinct |
| Conditional LAN challenge during phone/QR login | Implemented experimentally; live verification pending | Observed risk-extension state11, CGI22312 upload and length-prefixed token/scene reply. Synthetic wire, simulated-phone, upload deduplication, cancellation and expiry checks; actual phone/server acceptance, routing and fallback policy remain unverified |
| Admission-bound auth epochs and persistent logout inhibition | Implemented | Late authorization cannot cross a committed logout epoch |
| One-attempt native logout and explicit local forgetting | Implemented | Controlled native logout returned success; the same old authorization passed a heartbeat before logout and was rejected with -13 afterward, without renewal. Idempotent replay did not resubmit. Saved-device trust revocation remains unverified |
| Native push, persistent heartbeat and NewSync incoming detection | Implemented | Controlled standalone push with polling disabled matched the exact sent message ID, with a 1.366-second notification and 254-ms sync. The packaged Linux x64 daemon committed a new live message in 1.827 seconds and delivered it to the isolated Gateway model exactly once. Both architectures pass native tests. Stream liveness has bounded idle waits and durable-cursor replay; reconnect/restart receipts are recorded in the implementation PR |
| Durable original sync staging and official-file projection barrier | Implemented | Fault/retry fixtures; unsupported commands remain staged |
| Text, metadata, media and local read-state official writeback | Implemented for supported schemas | Supported encrypted-family fixtures, accepted outgoing history rows and image/voice/file/video cache/resource joins pass, including cold reads. FTS/search-index maintenance, unknown packed resources and arbitrary official schemas are outside the supported writers; existing unknown files are retained |
| Independent local image keys, DAT encoding and hardlink hashes | Implemented | Identity-derived image keys decrypt all 185 retained image files; native hash rule matches 84 hardlink records; fresh image cache projection also checked |
| Cursor-bound online resource pagination | Implemented | Persisted signed views, independent key, expiry/mutation tests and typed tombstone contracts |
| Durable event replay/SSE and independent consumer acknowledgements | Implemented | Cursor gap/account scope/restart tests, actual CLI emission checkpoint, real pinned Gateway SSE cancellation |
| Metadata catalogs distinct from message threads | Implemented | Exact group lookup succeeds without an existing thread and does not fabricate a visible conversation |
| Known-ID contact/group lookups and roster lookup without messages | Implemented | Controlled native contact/group lookup and seven-member roster retrieval passed, with sparse official-file projection |
| Contact/group sync checkpoints and avatar refresh/cache | Implemented | Separate durable streams and recovery fixtures; native group-sync page and small-avatar retrieval passed. Native contact inventory resumed across stored checkpoints and drained its catalog; three explicitly unavailable contacts remain reported as partial hydration. Their rejection does not imply deletion |
| Whole-thread reads, optimistic version checking and cross-device notification | Implemented | Frozen local ordering boundary, expected-version check, local clear watermark and durable one-attempt native notification; accepted, rejected and uncertain outcomes remain distinct. Reading a thread includes earlier messages that were not model input. Strict incoming opcode 1 controls have a separate local projection. Recognized opcode 5 controls can clear unread state once for existing visible ordinary sessions; interrupted attempts remain uncertain, and the complete session-exit control remains blocked. Local cutoffs use whole seconds; same-second arrivals and live phone comparison need separate validation |
| Incoming image qualities, normal/original video, file, short voice | Implemented | Three distinct JPEG qualities passed native download/hash/HTTP range checks. Normal H.264 video (720×960, 882,298 bytes) and original HEVC (1440×1080, 10,034,538 bytes) downloaded and decoded. An incoming PDF advertising file encryption version 0 downloaded through the native CDN with exact advertised size and MD5; discovery fixtures retain strict checks for versions 0/1 and reject unknown/malformed metadata. Broader media/playback coverage remains separate |
| Incoming WXGF source retention and representative still previews | Implemented for observed v2 containers | Twelve saved official samples decoded through the shared Rust/FFmpeg path and exactly re-encoded to their DAT bytes. Synthetic original/standard/thumbnail cache tests retain WXGF sources; Gateway fixtures verify a separately hashed JPEG reaches vision. Source animation/timing/alpha and official-client rendering remain unverified |
| Eligible incoming original sticker bytes, animation/integrity bounds | Implemented | Restricted pinned source, size/integrity and complete-frame fixtures; no sticker-send substitution or pack support |
| Durable outgoing text with idempotency and uncertain outcome | Implemented | Controlled File Transfer acceptance and lost-response/restart fixtures |
| Received shared-card metadata and optional thumbnails | Implemented; live verification pending | Plain/compressed encrypted-profile and group fixtures restore subtypes3/4/5 title/description/URL/source name. Optional image thumbnails use sidecar-only bounded CDN/public HTTP retrieval; transport credentials stay private. Card text survives unavailable previews; official card cache joins and shared music/video playback are not implemented |
| Outgoing URL shares, CLI and OpenClaw share tool | Implemented; live verification pending | New `share` request kind uses subtype5 app-message submission, the encrypted durable outbox, same-key conflict/restart guards and accepted-history projection. CLI `--share` and current-conversation `wechat_send_share` supply explicit metadata and optional thumbnail URL. Server acceptance and recipient/official-client rendering have not been verified |
| Incoming recalls and durable message suppression | Implemented for recognized recall controls and supported rows | Controlled external-party text recall preserved exact IDs/order, projected a notice and emitted one event. Out-of-order/replay, malformed controls, retained views, quotes and cached-media fixtures; original encrypted journals, unknown FTS and cached files remain retained |
| Own-message native recall, CLI and scoped OpenClaw tool | Implemented | Shipped Linux x64 codec evidence, durable one-attempt/uncertainty fixtures and real Gateway send-receipt/tool replay. Controlled own-text/file recalls returned server acceptance and recipient phone notices; the file's captured media handle returned HTTP 410 before and after restart, and key replay retained one operation/event. Other native-machine live acceptance remains separate |
| Outgoing image/file/video/independently encoded voice | Implemented | Fresh native File Transfer submissions accepted all four kinds and committed history rows; all four supported official caches/resource joins complete after local repair. CDN readback checked before final CGI; the account owner confirmed the latest file/image open and video/voice playback |
| Voice splitting and per-note partial/uncertain receipts | Implemented | Independent codec,10-second chunks, per-note IDs/status and stop after uncertainty; single-note phone playback confirmed; multi-chunk playback remains unverified |
| Bounded local media writeback repair | Implemented | Actual CLI repaired nine retained candidates with zero blocked; all four accepted media sends now report complete. Uses owned bytes/receipts without a transfer or resend |
| Local source staging, upload bounds, media cancellation/retry | Implemented | Encrypted immutable sources, claim CAS, bounded retry/cancel/restart and atomic event rollback fixtures |
| CLI submission receipt before first request, lookup by key | Implemented | Actual ELF fixture inspects durable receipt before HTTP submission and proves same-key lookup/conflict |
| Cached content streaming and bounded HTTP range requests | Implemented | SDK bounds/hash checks and actual pinned Gateway binary206/content-range forwarding |
| Offline profile registration, inspection, catalog/history/media commands | Implemented | Direct library fixtures, source-view handles/redaction and CLI keyless inspection without an API token |
| Offline key export from explicit authorized cache/password material | Implemented | No process acquisition, password discovery or implicit native login |
| Source-preserving read snapshots and source-qualified IDs | Implemented | Main/WAL/SHM unchanged, source-generation cursors and namespace tests |
| Authoritative Rust contracts and generated TypeScript types | Implemented | Actual Rust media/source types, server response checks, stale-generation CI check, lossless IDs and tombstone fixtures |
| Native SDK bearer transport, bounded JSON/SSE and lossless IDs | Implemented | Redirect, account binding, large IDs, expiry and certificate checks |
| OpenClaw preinstalled executable preflight and managed/external modes | Implemented | Actual owned ELF/npm launcher smoke; no installer or Docker fallback |
| OpenClaw durable custody, replay dedup and access/mention gates | Implemented | Recorded model harness; catch-up/import events do not create new turns |
| Inbound handling and whole-thread read policy | Implemented for pinned host boundary | Default `on_inbound` queues a whole-thread read after normal inbound completion, including commands without a model and without conversation-access permission. Ignored messages, pending media and failed handling do not initiate reads. Actual OpenClaw2026.10.2 Gateway/local-model checks cover text, concurrent turns, media/transcription, pending/failed content, durable read recovery and duplicate prevention. Explicit `on_model_dispatch` retains exact input/asset confirmation and requires compatible host context plus conversation permission; `never` disables read projection. A whole-thread read also clears earlier messages absent from model input. Server acceptance does not prove a phone badge changed or media was semantically analyzed |
| OpenClaw native attachment paths, voice and outbound media | Implemented | Integrity/pending/expired fixtures and correlated model harness; gates precede retrieval/transcription, ordinary audio remains a file |
| Optional authenticated Gateway HTTP facade | Implemented | Actual pinned OpenClaw2026.10.2 Gateway plugin load/auth, separate tokens, account restriction, Range206 and SSE/cancellation; default disabled |
| Verified exact-version npm/native artifacts and offline provisioning | Implemented | Linux x64/ARM64 GNU validation; macOS ARM64 and Windows x64/ARM64 native build/package/lifecycle candidate jobs. Native-machine live account acceptance remains pending; see `dev-notes/native-platform-acceptance.md` |
| Fail-closed release gates and source/tag/version integrity | Implemented | Mock registry/tag tests; existing published versions cannot be repurposed |
| Standalone Docker, systemd and Compose assets | Implemented | Private persistent data/scratch, no official runtime; examples never deployed |

A public fixture result is never substituted for recipient playback. Unknown
sync commands stay staged and block publication of a complete official cursor;
native receipt of a page does not mean every official file has caught up.
Unknown packed resource subtypes, arbitrary avatar replacement and FTS/search
index maintenance remain compatibility gaps. Copying a profile does not repair
those gaps.

## Experimental extras outside the initial scope

| Extra | Current evidence / limits |
| --- | --- |
| Fresh official-format profile initialization | Native login created seven encrypted databases, MMKV identity and saved login; native sync and all four media downloads passed. The exact official build accepted saved login with matching identity/password, but complete first-time initialization and rendering were not established |
| Copy-only stopped-profile import/export | CLI fixtures preserve opaque bytes, require a stopped source, exclude cooperating owners, refuse pending projections and report explicit incomplete/checkpoint receipts. This is not a schema converter or a guarantee the official app can continue |
| Returning a native-written profile to the official app | Controlled official saved-login and subsequent native encrypted-history readback passed. Official UI rendering and FTS/search compatibility remain unverified; not a v1 acceptance requirement |

## Explicit follow-ups outside the initial capability claim

- Group creation, invitation, joining/leaving and rename mutations require a
  separately approved destination/member experiment and uncertainty handling.
- A web application follows stabilization of the API, events and media surface.
- Large-file variants, sticker sending/packs, official macOS/Windows profile compatibility and
  arbitrary official-build migration require independent evidence and testing. A controlled
  live check on 2026-10-22 sent 40 MiB file and 40 MB video sources through the existing
  upload path: the CDN returned HTTP 400 on the first piece, nothing was sent, and the jobs
  ended `needs_review` with `serviceStage: http`. Sources above 25 MiB need the separate
  large-file upload branch.
- Incoming status notifications (message type 51) with operation IDs 2, 11 and 23, the
  session-exit part of opcode 5, and type 10002 `functionmsg`/`dynacfg` controls arrive
  during normal live use and remain staged as `unsupported_sync_control`. As a result
  `pendingProjectionBatches` grows and `pendingCursorProjection` stays true. New messages
  still commit as `live`. Each needs its official handler established, then applying or an
  explicit no-local-effect classification.
- WXGF encoding/sending, source animation timing and faithful alpha rendering
  require separate evidence. Current incoming support preserves the container
  and supplies a representative still preview.

## Release gate

Before describing an initial capability as complete, exercise its actual daemon
API and CLI, retain a reproducible fixture or controlled test receipt, verify
restarts and failure paths, and update this table. User-visible playback and
phone approval remain distinct from a successful server response. Current local
artifacts are validation inputs, not a published or deployed release.
