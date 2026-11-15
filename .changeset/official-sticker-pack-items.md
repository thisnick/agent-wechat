---
"@agent-wechat/rust-crates": patch
---

Official sticker packs such as WeChat Bubble Pup now download. Pack archives are now requested from WeChat's CDN as storage downloads, the way the official client requests them. Before, they were requested as ordinary file downloads, and the CDN refused some packs' archives with HTTP 400, so a pack or sticker download failed with `media_service_rejected`.
