---
"@agent-wechat/cli": patch
---

Fix image sends that fail when an automatically generated JPEG variant is larger than its source. Generated variants now fit within the source dimensions and the existing byte limit.
