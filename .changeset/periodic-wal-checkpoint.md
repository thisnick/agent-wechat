---
"@agent-wechat/wechat": patch
---

Add periodic WAL checkpoint for fresh WeChat DB reads. A background task runs PASSIVE checkpoint every 3s, flushing WAL to the main DB file so immutable=1 reads see up-to-date data.
