---
"@agent-wechat/client-ui": patch
"@agent-wechat/agent-wechat": minor
"@agent-wechat/shared": minor
---

Make profile creation, sign-in, sign-out and removal available from the account chooser without a separate frontend management mode. Keep the daemon key server-side in the connected preview. Require a typed profile name for removal; the API unregisters only signed-out, idle profiles and retains their files and recovery records.
