---
"@agent-wechat/rust-crates": patch
---

Official sticker packs such as WeChat Bubble Pup now download. WeChat's CDN refuses these packs' archives with HTTP 400, so a pack or sticker download failed with `media_service_rejected`. When the CDN refuses a pack archive, the daemon now fetches each sticker from the source the pack's details list for it, checks every original against its sticker ID (an MD5), and saves the pack as before, including the profile copy for added packs. Packs whose archive downloads normally are unchanged.
