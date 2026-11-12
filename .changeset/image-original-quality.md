---
"@agent-wechat/cli": minor
"@agent-wechat/rust-crates": minor
---

Images can now be sent at full resolution. Pass `--image-quality original` to `wx messages send --image`, or `"imageQuality": "original"` to `POST /sends`. A JPEG is then sent unchanged as the image's original copy, whatever its resolution, and other formats are converted to JPEG once at full resolution. Without the option, images are prepared as before, with the original copy reduced to at most 1,280 pixels and 65,520 bytes.

JPEG images up to 8,192 pixels per edge and 64 million pixels can now also be downloaded and read back; previously images wider or taller than 4,096 pixels were rejected.
