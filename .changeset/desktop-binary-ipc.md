---
"@agent-wechat/desktop": patch
"@agent-wechat/client-ui": patch
---

Stream desktop HTTP proxy uploads as raw binary Tauri IPC chunks instead of JSON byte arrays, and stream responses as binary frames instead of base64 strings.
