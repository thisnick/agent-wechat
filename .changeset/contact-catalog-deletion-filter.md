---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": minor
---

Exclude deleted rows from the saved contact catalog when the official profile exposes a deletion marker. Keep nullable markers and readable layouts without that column working, and preserve historical chat labels and group rosters.

Add `ProfileReader::contact` for exact reads of retained contact and group rows. Group admission uses this lookup so hiding deleted catalog entries preserves its existing eligibility checks and errors. Ambiguous stored usernames are rejected.
