---
"@agent-wechat/client-ui": minor
"@agent-wechat/shared": patch
"@agent-wechat/cli": patch
---

Add a New chat dialog with direct-message contact selection and a simulator-backed group creation flow. Retain creation keys and receipts across reload, recover lost responses through status lookup, and keep uncertain or interrupted requests from creating a second group. Project synthetic group identity, notices and member rosters through the real hub, with account-scoped text and media destinations.

Expose a typed group request and admin-only operation API. Native group creation remains disabled and advertises no support until its service implementation is validated. Existing native roster refresh continues to use its original protocol path. Use “agent-wechat hub” throughout browser connection screens, settings, status labels and errors.
