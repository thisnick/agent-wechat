---
"@agent-wechat/client-ui": minor
"@agent-wechat/shared": patch
"@agent-wechat/cli": patch
---

Add a New chat dialog and group management through the native group operation API. Create groups with an optional name, rename, add/remove members, change your display name, mute/unmute notifications, clear announcements and leave while keeping cached history. Disable new text and attachment sends after a confirmed leave. Show partial member acceptance and uncertain outcomes, retain account/target-scoped keys across reload and recover status without resubmitting a change.

Refresh cached group details and rosters from conversation details and group events. Extend the isolated simulator at the native RPC boundary so the same admission, journal, response validation and profile projection run in preview tests. Preserve the native large-file/video transfer paths when adapting the simulator transport. No official endpoint is used for UI validation. Use “agent-wechat hub” throughout browser connection screens, settings, status labels and errors.
