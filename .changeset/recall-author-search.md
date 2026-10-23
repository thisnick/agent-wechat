---
"@agent-wechat/agent-wechat": patch
"@agent-wechat/cli": patch
"@agent-wechat/shared": patch
---

Match official recall rows and notices, derive API sender and direction from existing message and receipt evidence without new author columns, and remove recalled entries from supported official message search indexes. Keep unknown authors unset and leave unsupported index layouts pending for repair.
