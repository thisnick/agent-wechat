---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": minor
---

Keep blocked sync batches and the event log from growing, and apply more session-entry controls.

- **Session entry.** A type-51 session-entry control (status opcode 2) now completes for a group or an ordinary contact once its unread clear is written, and for the built-in session holders the official Linux client ignores. Service accounts the official client's account classifier recognizes, `notifymessage`-style aggregates and hidden sessions still keep their batch blocked, because the official client does more for them. Session exit (opcode 5) and the other unmapped controls stay blocked.
- **Blocked batches.** A blocked batch outside the newest 256 keeps only the commands not yet written to the profile, plus contact records. Each blocked status or system control leaves a small record of its kind and peer type (never the peer), and `wx store usage` lists them under `blockedControls`.
- **Attachments never downloaded.** Message attachment rows that were listed but never downloaded, and aren't queued for automatic download, are removed after 7 days. Voice rows are kept. Reading one returns `operation_expired` (410) until listing the message's media again recreates it with the same ID.
- **Event log.** Consecutive `job.updated` events for the same job that differ only in transfer progress replace each other, so the 10,000-event log covers more time. No state is lost: the newest event for each state a job passed through stays.
