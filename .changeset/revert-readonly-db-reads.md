---
"@agent-wechat/wechat": patch
---

Revert READ_ONLY + busy_timeout DB reads back to immutable=1 with WAL checkpoint task. The READ_ONLY approach from #53 did not work as expected.
