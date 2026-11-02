---
"@agent-wechat/cli": patch
---

Fix group creation, member additions, and pin/mute preferences for contacts whose saved deletion flag is NULL. New contacts created by sparse updates default to a zero deletion flag while existing deletion state is preserved.
