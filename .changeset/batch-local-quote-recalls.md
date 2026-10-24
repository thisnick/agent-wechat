---
"@agent-wechat/agent-wechat": patch
"@agent-wechat/cli": patch
---

Batch local recall checks for quoted originals when reading history pages. Reuse
one page-scoped journal lookup and grouped profile queries instead of repeating
database reads for every quote. Preserve embedded excerpts, recall suppression,
conversation boundaries and fresh checks on subsequent requests.
