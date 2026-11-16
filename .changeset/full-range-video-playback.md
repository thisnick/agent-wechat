---
"@agent-wechat/agent-wechat": patch
---

Full-range videos (for example `yuvj420p`, or `yuv420p` tagged `color_range=pc`) can now be sent at normal quality on hosts with FFmpeg 7 or later. The playback copy is converted to limited range; before, it kept the source's full range and the daemon rejected its own copy with `media_processing_failed`.
