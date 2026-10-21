---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Accept complete images with trailing source data while preserving downloaded bytes and integrity hashes. Use format-specific JPEG, PNG, GIF and WebP boundaries, decode static WebP images correctly, retain animated frames, and report fixed image-validation reasons instead of a generic media-processing failure.

Read supported incoming WXGF containers through a bounded FFmpeg decoder, retaining original bytes in the server and official DAT cache. Provide separately encrypted, source-bound JPEG still previews for OpenClaw vision and quote retrieval. Source animation timing/transparency and sticker sending remain outside this change.

Download incoming files advertising attachment encryption version 0 through the verified native CDN path, preserving strict advertised size/checksum validation. Report unsupported or malformed attachment metadata as specific media errors instead of `unsupported_profile`; PDFs remain byte-preserving file-path references in OpenClaw.
