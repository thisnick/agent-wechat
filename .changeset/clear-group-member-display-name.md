---
"@agent-wechat/rust-crates": minor
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

Clear saved group member display names and opaque member fields in `chat_room.ext_buffer` when a roster refresh omits them for returned members, matching the official Linux client while preserving unchanged delta members and unknown extension chunks. Also omit field 2 locally when clearing your own group display name via `MyName`, and add `wechat_profile::{NativeRosterMember, NativeRosterUpdate, apply_native_roster_update}`.
