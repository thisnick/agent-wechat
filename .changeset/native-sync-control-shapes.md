---
"@agent-wechat/cli": patch
---

Accept 64-bit last-message timestamps in incoming read controls while retaining the notification timestamp as the unread cutoff. Preserve the once-only partial unread effect and the remaining session-exit checkpoint barrier.

Apply the supported unread-clear portion of incoming opcode-2 chat-entry controls once for existing visible ordinary chats. Preserve newer unread messages, uncertain-attempt markers and the complete-control checkpoint barrier. Report successful entry read changes through `thread.read-state` with `origin: remote_entry_read` and `completeControl: false`.

Recognize narrowly verified Linux control forms without local profile effects: status opcodes 3, 15 and 16, system ClientCheckGetExtInfo and mmchatroombarannouncememt entries, and self-to-self type-10002 appmsg subtype 35. Preserve encrypted original bytes and deduplicated classification receipts. Normalize validated incoming group sender prefixes before parsing supported recalls; unsupported recall fields and other controls remain pending.
