---
"@agent-wechat/cli": patch
---

Avoid repeated full-profile snapshots during startup by recovering only unfinished recall writes. Completed recall history is no longer rechecked after restart or while reading restored originals.
