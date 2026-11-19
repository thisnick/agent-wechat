---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": minor
---

Apply status controls the official client has no handler for, and stop keeping expired sticker store previews.

- **Status controls without a handler.** A type-51 status control now completes when every operation in it uses an ID the official Linux client has no handler for (anything other than 1, 2, 4, 5, 7, 9, 11, 13, 14 and 21), whatever the operation contains or who sent it. Before, only the recorded forms of opcodes 3, 15, 16 and 23 completed, so other forms such as opcode 24 kept their sync batch blocked. A control that also carries an operation with a handler still blocks.
- **Sticker store previews.** Pack icons, covers and sticker thumbnails listed from the sticker store are removed from `control.sqlite` once their one-day window ends, unless a download for them is queued or running. Reading a removed preview returns `not_found` (404), as an expired one could no longer be downloaded or read anyway; browsing the store again lists new ones. Before, each day's listing added rows that were never removed.
