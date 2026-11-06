---
"@agent-wechat/cli": major
---

Make the Rust client own its account through `NativeClient::from_account`.
Cloned client handles share accepted credentials, and async sync, heartbeat,
text and call requests use temporary snapshots without locking across network
waits. Explicit renewal fences prior credential generations; shutdown and
one-time state handoff prevent late replies from restoring stopped credentials.

Add shared-clock PCM mixing with per-track queue budgets, microphone controls,
generation fencing, silence for missing sources and frame peak limiting.
The daemon uses the same account owner for ordinary requests, notification
polling and call signaling, while preserving encrypted journals and verified
receipts across credential-storage failures. Conference runtime and live
acceptance remain in progress.

Move direct/incoming/conference session state machines into `wechat-calls`,
with `native-runtime` as its default feature and a portable no-default build.
Keep the daemon's private PCM envelope in Core. Add guarded conference creation,
additional invitations, partial receipts and creator cancellation/leave state;
these paths still require the isolated API/CLI acceptance milestone.

Keep leave requests valid across conference roster updates and avoid duplicate exit requests after confirmed termination.

Accept the creator's supported voice channel configuration without cancelling pending invitations when its channel health settings differ from a joiner's.

Allow a verified empty SQLite WAL and rebuilt empty index during sidecar startup, retaining a recovery copy and validating the database before opening it.

Keep legacy conference audio receivers running through authenticated nonce reuse, matching direct-call compatibility. Count reuse by packet family while retaining sender authentication, replay filtering and outgoing counter limits.

Extract direct and conference media loops, relay setup, participant source tracking and interruption handoff gates into `wechat-calls`; retain daemon authorization, event conversion and encrypted journals in Core.
