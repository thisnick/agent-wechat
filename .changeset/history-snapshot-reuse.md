---
"@agent-wechat/agent-wechat": patch
"@agent-wechat/cli": patch
---

Reuse a history request's captured profile snapshot when checking quoted-message recalls, avoiding repeated full-profile copies and hashing. Capture unfiltered history membership without reading message bodies, then load and normalize only the delivered page. Preserve fresh reads between requests, content search and recall suppression.
