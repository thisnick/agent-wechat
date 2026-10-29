---
"@agent-wechat/cli": patch
"@agent-wechat/client-ui": patch
---

Recognize MOV, 3GP, 3G2 and MJ2 attachments as videos and prepare them as MP4 playback, including older QuickTime movies without an `ftyp` header. Add an Original video choice in the desktop and web attachment review to include the unchanged source alongside playback. Use the verified container type for original-video download extensions and show a fallback when the browser cannot preview a selected video.
