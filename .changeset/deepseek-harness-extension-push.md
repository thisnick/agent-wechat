---
'@agent-wechat/deepseek-harness-extension': minor
---

Add the `wechat_push` message-push tool: when `push.enabled` is true with at
least one `push.targets` entry, every DSH session (including non-WeChat
agents) can send one-way notification messages to the configured WeChat ids
or group ids. Targets outside the whitelist are rejected; delivery rides the
durable outbox with canonical readback and `needs_review` semantics. Also
adds push fields to the visual settings page, and gives every wxid / group-id
field a directory search picker backed by a new host-side, read-only
`/plugins/wechat-channel/search` route.
