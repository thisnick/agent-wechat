---
'@agent-wechat/deepseek-harness-extension': minor
---

Add a visual settings page for the WeChat channel plugin: a client half
registers the bundle in the Settings panel (微信通道 nav page) and on its
Plugins page, with an editable form for hub address, run mode, DM/group
admission, owner allowlist, tool grants, and the calls block. Edits are
validated against the plugin Config schema and written to the profile patch;
overridden fields get a reset control and secrets are never displayed. Both
surfaces also show a live connection status (polled from a read-only
`/plugins/wechat-channel/status` route on the host web server).
