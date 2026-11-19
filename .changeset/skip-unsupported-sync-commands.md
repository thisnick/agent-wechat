---
"@agent-wechat/cli": minor
"@agent-wechat/rust-crates": minor
---

Stop letting commands the daemon can't apply hold back the sync position.

- **Skipped, not blocked.** A sync command this build can't apply no longer keeps its batch blocked. This covers status and system controls with no supported effect (including session exit, status opcode 5), updates to your own profile, unknown commands, and message types the profile writer doesn't support, such as contact cards. Deleting an existing contact through an unsupported path, and a recall in a form the daemon doesn't accept, still wait, so neither leaves stale content behind. The daemon applies whatever it supports first; a chat-open or chat-exit notice still clears unread state. Then it keeps the command's raw bytes, emits `account.control_observed` with `projection: "skipped"` and a label of its kind, and lets the batch finish, so the profile's saved sync position moves on. The raw bytes and a note of what was skipped are kept, so a later version can apply them; `wx store usage` lists them under `skippedControls`.
- **Still waiting.** A command that waits on other local work, such as a group refresh, sticker reconciliation or a contact update with unresolved fields, still holds its batch and emits `projection.blocked`.
- **Old blocked batches retry on start.** When the daemon starts, it replays batches an earlier version left blocked, the same way `wx store recover-projection` does, so they finish without a manual step.
