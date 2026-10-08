---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/wechaty-puppet": patch
"@agent-wechat/wechaty-gateway": patch
"@agent-wechat/agent-server": patch
---

Fix GitHub OIDC release publishing by ensuring pnpm uses the pinned, OIDC-capable npm instead of Node's bundled npm. Add a publishing-subprocess dry-run check so an incorrect npm version fails before publication.
