---
"@agent-wechat/wechat": patch
---

Fix stale WeChat DB reads by replacing immutable=1 with READ_ONLY + busy_timeout. WeChat DBs likely use DELETE journal mode where immutable=1 skips change-detection entirely. Also adds journal_mode logging to confirm the actual mode.
