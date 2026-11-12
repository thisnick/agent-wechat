---
"@agent-wechat/cli": minor
"@agent-wechat/rust-crates": minor
"@agent-wechat/agent-wechat": patch
---

Images can now be sent at full resolution. Pass `--image-quality original` to `wx messages send --image`, or `"imageQuality": "original"` to `POST /sends`. A JPEG within the supported source limits is then sent unchanged as the image's original copy, and other formats are converted to JPEG once at full resolution. The CLI and HTTP API default to `standard`, offering only compressed standard and thumbnail copies without an Original download option. Existing saved upload receipts retain their previous original availability. OpenClaw image sends automatically request original quality and allow large image transfers and delayed local receipts to finish without resubmitting the send.

JPEG images up to 8,192 pixels per edge and 64 million pixels can now also be downloaded and read back; previously images wider or taller than 4,096 pixels were rejected.
