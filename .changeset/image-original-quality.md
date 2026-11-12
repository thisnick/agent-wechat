---
"@agent-wechat/cli": minor
"@agent-wechat/rust-crates": patch
---

Images can now be sent with the full-resolution original instead of the 640-pixel preview. Pass `"imageQuality": "original"` to `POST /sends`, or `--image-quality original` to `wx messages send --image`. The original copy keeps source resolution up to 4096 pixels and is allowed to exceed the 65,520-byte limit used by the standard and thumbnail copies, so recipients can open the real original. Omitting the field keeps today's compact behavior.
