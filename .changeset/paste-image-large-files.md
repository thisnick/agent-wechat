---
"@agent-wechat/cli": patch
"@agent-wechat/wechat": patch
---

Fix sending large images via clipboard: scale pre-paste delay by file size so xclip has time to buffer before Ctrl+V fires.
