---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
---

Add normal/original video sends for MP4 sources through 50 MiB. Prepare and verify a compressed playback copy before uploading; original mode also uploads and verifies the unchanged source. Save prepared bytes and separate upload identities for recovery, preserve uncertain send handling, and expose videoQuality through the HTTP API, SDK, CLI --video-quality and OpenClaw message channelData. Honor OpenClaw forceDocument/asDocument to send media as ordinary files.
