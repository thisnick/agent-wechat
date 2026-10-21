# Native client implementation checklist

This checklist tracks implementation and validation separately. A command or
route appearing in help is not evidence of a successful native exchange. The
runtime remains experimental until the required integration gates below pass.
No deployment or package publication is part of this implementation task.

Legend: **Implemented** means executable code and focused tests exist;
**Partial** identifies an unfinished required behavior; **Pending** identifies
work still required. Live validation is stated separately and never inferred
from fixture tests. Capability responses must reflect these limits.

| Required initial capability | Implementation | Validation / remaining work |
| --- | --- | --- |
| Independent protocol, profile, store, core and optional HTTP crates | Implemented | Library consumers and offline synthetic fixtures |
| Versioned configuration, XDG paths, protected registry/sidecars | Implemented | Precedence and unsupported-version tests |
| Separate server configuration and endpoint-scoped client defaults | Implemented | Actual CLI scoped endpoint/default smoke and ambiguity fixtures |
| Canonical daemon account aliases | Implemented | Actual daemon creation, rename, default selection and ambiguous selection checks |
| Simultaneous independent account actors | Implemented | Synthetic isolation; two independent live accounts remain a separate gate |
| Exclusive process/account/profile ownership | Implemented | Canonical database-root overlap regression; unmanaged official client must still be stopped explicitly |
| Foreground `serve`, background start/status/stop/logs | Implemented | Actual ELF background start/status/stop/restart with PID/store generation and occupied-owner checks |
| Protected active/saved credentials and independent API token rotation | Implemented | Restart, permission, redaction and cryptographic integrity tests |
| Active authorization validation, bounded renewal and saved-device recovery | Implemented | Verified native exchanges are separate from universal lifetime/expiry claims |
| Single-switch official-file login and scoped HOME exports | Implemented | Account selection, identity/password admission, file-preservation and scoped-copy fixtures; live one-command takeover validation remains separate |
| Explicit QR login, white PNG, scan/approval, cancellation, bounded refresh | Implemented | Pixel-grid white renderer test; actual CLI generation/PNG/interrupt fixture; controlled native account validation remains distinct |
| Admission-bound auth epochs and persistent logout inhibition | Implemented | Late authorization cannot cross a committed logout epoch |
| One-attempt native logout and explicit local forgetting | Implemented | Controlled native logout returned success; the same old authorization passed a heartbeat before logout and was rejected with -13 afterward, without renewal. Idempotent replay did not resubmit. Saved-device trust revocation remains unverified |
| Heartbeat/NewSync incoming detection | Implemented | Native polling is distinct from unresolved fast push association |
| Durable original sync staging and official-file projection barrier | Implemented | Fault/retry fixtures; unsupported commands remain staged |
| Fresh official-format profile initialization | Implemented | Native saved-device approval without QR supplied the accepted database password and created seven encrypted databases plus login storage; stock-client takeover/rendering remains a separate gate |
| Text, metadata, media and local read-state official writeback | Partial | Supported encrypted-family fixtures, accepted outgoing history rows and image/voice/file/video cache/resource joins pass, including cold reads; FTS/index maintenance and stock-client rendering remain gates |
| Independent local image keys, DAT encoding and hardlink hashes | Implemented | Identity-derived image keys decrypt all 185 retained image files; native hash rule matches 84 hardlink records; fresh image cache projection also checked |
| Cursor-bound online resource pagination | Implemented | Persisted signed views, independent key, expiry/mutation tests and typed tombstone contracts |
| Durable event replay/SSE and independent consumer acknowledgements | Implemented | Cursor gap/account scope/restart tests, actual CLI emission checkpoint, real pinned Gateway SSE cancellation |
| Metadata catalogs distinct from message threads | Implemented | Exact group lookup succeeds without an existing thread and does not fabricate a visible conversation |
| Known-ID contact/group lookups and roster lookup without messages | Implemented | Controlled native contact/group lookup and seven-member roster retrieval passed, with sparse official-file projection |
| Contact/group sync checkpoints and avatar refresh/cache | Implemented | Separate durable streams and recovery fixtures; native group-sync page and small-avatar retrieval passed. Native contact inventory resumed across stored checkpoints and drained its catalog; three explicitly unavailable contacts remain reported as partial hydration. Their rejection does not imply deletion |
| Sparse exact-ID desktop reads, optimistic version checking | Implemented | Gaps/concurrent-arrival preservation; does not synchronize phone read state |
| Explicit human mark-through read boundary | Implemented | Local ordering boundary plus expected version; SDK/CLI keep it separate from exact model IDs |
| Incoming image qualities, normal/original video, file, short voice | Implemented | Three distinct JPEG qualities passed native download/hash/HTTP range checks. Normal H.264 video (720×960, 882,298 bytes) and original HEVC (1440×1080, 10,034,538 bytes) downloaded and decoded. Broader media/playback coverage remains separate |
| Eligible incoming original sticker bytes, animation/integrity bounds | Implemented | Restricted pinned source, size/integrity and complete-frame fixtures; no sticker-send substitution or pack support |
| Durable outgoing text with idempotency and uncertain outcome | Implemented | Controlled File Transfer acceptance and lost-response/restart fixtures |
| Outgoing image/file/video/independently encoded voice | Implemented | Fresh native File Transfer submissions accepted all four kinds and committed history rows; all four supported official caches/resource joins complete after local repair. CDN readback checked before final CGI; phone playback confirmation remains pending |
| Voice splitting and per-note partial/uncertain receipts | Implemented | Independent codec,10-second chunks, per-note IDs/status and stop after uncertainty; live note playback remains required |
| Bounded local media writeback repair | Implemented | Actual CLI repaired nine retained candidates with zero blocked; all four accepted media sends now report complete. Uses owned bytes/receipts without a transfer or resend |
| Local source staging, upload bounds, media cancellation/retry | Implemented | Encrypted immutable sources, claim CAS, bounded retry/cancel/restart and atomic event rollback fixtures |
| CLI submission receipt before first request, lookup by key | Implemented | Actual ELF fixture inspects durable receipt before HTTP submission and proves same-key lookup/conflict |
| Cached content streaming and bounded HTTP range requests | Implemented | SDK bounds/hash checks and actual pinned Gateway binary206/content-range forwarding |
| Offline profile registration, inspection, catalog/history/media commands | Implemented | Direct library fixtures, source-view handles/redaction and CLI keyless inspection without an API token |
| Offline key export from explicit authorized cache/password material | Implemented | No process acquisition, password discovery or implicit native login |
| Source-preserving read snapshots and source-qualified IDs | Implemented | Main/WAL/SHM unchanged, source-generation cursors and namespace tests |
| Copy-only stopped-profile attach/import/export | Implemented | Actual CLI fixtures preserve opaque bytes, require stopped-source assertion, exclude another native owner across roots, refuse all pending projection categories and report explicit incomplete/checkpoint receipts |
| Official client forward/backward handoff | Partial | Offline structure tests do not replace exact-build controlled runtime checks |
| Authoritative Rust contracts and generated TypeScript types | Implemented | Actual Rust media/source types, server response checks, stale-generation CI check, lossless IDs and tombstone fixtures |
| Native SDK bearer transport, bounded JSON/SSE and lossless IDs | Implemented | Redirect, account binding, large IDs, expiry and certificate checks |
| OpenClaw preinstalled executable preflight and managed/external modes | Implemented | Actual owned ELF/npm launcher smoke; no installer or Docker fallback |
| OpenClaw durable custody, replay dedup and access/mention gates | Implemented | Recorded model harness; catch-up/import events do not create new turns |
| Exact prepared-model read receipts | Partial | Actual pinned OpenClaw2026.10.2 Gateway/local-model text proof passes with explicit host conversation permission, exact run/input facts and model-call correlation; missing permission and never-read behavior checked. Media on that host remains unconfirmed; merged-input rejection has unit coverage |
| OpenClaw native attachment paths, voice and outbound media | Implemented | Integrity/pending/expired fixtures and correlated model harness; gates precede retrieval/transcription, ordinary audio remains a file |
| Optional authenticated Gateway HTTP facade | Implemented | Actual pinned OpenClaw2026.10.2 Gateway plugin load/auth, separate tokens, account restriction, Range206 and SSE/cancellation; default disabled |
| Verified exact-version npm/native artifacts and offline provisioning | Implemented | Linux x64/glibc2.36 package/image/owned-host smoke |
| Fail-closed release gates and source/tag/version integrity | Implemented | Mock registry/tag tests; existing published versions cannot be repurposed |
| Standalone Docker, systemd and Compose assets | Implemented | Private persistent data/scratch, no official runtime; examples never deployed |

The metadata and projection surfaces still require their
controlled account/stock-client acceptance gates before their rows can move from
Partial. A public fixture result is never substituted for recipient playback.
Unknown packed resource subtypes, arbitrary avatar replacement and FTS/index
maintenance remain compatibility gaps. Copying a profile does not repair those
gaps; incomplete media writeback requires an explicit incomplete export.

## Explicit follow-ups outside the initial capability claim

- Group creation, invitation, joining/leaving and rename mutations require a
  separately approved destination/member experiment and uncertainty handling.
- A web application follows stabilization of the API, events and media surface.
- Fast unsolicited native push association is separate from verified polling.
- Large-file variants, sticker sending/packs, additional architectures and
  arbitrary official-build migration require independent evidence and testing.

## Release gate

Before describing an initial capability as complete, exercise its actual daemon
API and CLI, retain a reproducible fixture or controlled test receipt, verify
restarts and failure paths, and update this table. User-visible playback and
phone approval remain distinct from a successful server response. Current local
artifacts are validation inputs, not a published or deployed release.
