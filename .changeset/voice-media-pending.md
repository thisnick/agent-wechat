---
"@agent-wechat/agent-server": patch
"@agent-wechat/wechat": patch
---

Return "pending" instead of "unsupported" when voice data is not yet available in the database, so the extension retries instead of giving up.
