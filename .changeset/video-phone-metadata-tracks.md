---
"@agent-wechat/agent-wechat": patch
---

Videos recorded on an iPhone can now be sent. Their timed-metadata tracks no longer count toward the source's stream limit, so a recording with several of them no longer fails with `media_processing_failed`; the playback copy still uses one video and one audio stream and leaves the metadata out.
