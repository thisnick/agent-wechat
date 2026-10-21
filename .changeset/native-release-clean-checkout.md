---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Keep generated release notes outside the build checkout so native release manifests correctly identify clean source. Reject dirty architecture fragments before publishing release assets, rather than waiting for the npm and image publication gates.
