---
"@agent-wechat/agent-wechat": patch
---

Videos with more than one audio track, or with embedded cover art, can now be sent at normal quality. The playback copy uses the first audio track and ignores the cover image; before, these sources failed with `media_processing_failed`. The original upload is unchanged.
