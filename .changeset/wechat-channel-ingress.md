---
"@agent-wechat/agent-wechat": minor
---

Decide WeChat access with OpenClaw's channel ingress policy instead of a local copy of it. DM and group sender gates, per-group overrides, control-command authorization and @mention gating now come from core, so `accessGroup:<name>` entries work in `allowFrom`, `groupAllowFrom` and per-group `allowFrom`. Existing configs keep their behavior, with two exceptions:

- The pairing store no longer admits group senders or authorizes commands. WeChat has no pairing flow, so the store is normally empty.
- `dmPolicy: "open"` without `"*"` in `allowFrom` still admits everyone, but now logs a reminder to add `"*"`.
