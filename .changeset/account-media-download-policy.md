---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
"@agent-wechat/shared": minor
"@agent-wechat/client-ui": patch
---

Download received image and video thumbnails and voice notes on the hub without
an open client. Automatically fetch other image, video, file and sticker variants
when their advertised size is within a per-account limit, defaulting to 50 MB.
Keep larger or unknown-size versions available on request.

Store the policy in the hub registry, separate from official profiles. Expose it
through account settings API/SDK methods, `wx account settings` and browser
settings. Include advertised size and current automatic-download eligibility in
media metadata; show thumbnails while larger image versions remain manual.
