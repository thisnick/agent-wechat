---
"@agent-wechat/wechat": patch
---

Fix @agent /command regex to support multi-word agent display names by using WeChat's hair space (U+2005) as the mention boundary instead of splitting on all whitespace
