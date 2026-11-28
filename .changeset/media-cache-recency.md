---
"@agent-wechat/client-ui": patch
"@agent-wechat/desktop": patch
---

Keep cached media and avatar recency metadata separate from their file bytes so opening a cached image, video, or avatar does not write its contents back to disk or load full payloads for cache stats and eviction. Local cache storage is upgraded to a new schema version (waiting up to 3 seconds if another open tab holds the previous version); installing an earlier version afterwards turns off local cache persistence (including reading saved drafts and pending-send receipts) until app data is cleared or you upgrade again.
