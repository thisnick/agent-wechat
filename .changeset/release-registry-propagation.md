---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/wechaty-puppet": patch
---

Allow releases to proceed after successful npm publication without waiting for registry propagation, and resume binary and Docker publication when npm packages already exist but the GitHub release is missing.
