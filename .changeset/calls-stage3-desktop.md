---
"@agent-wechat/desktop": minor
"@agent-wechat/client-ui": patch
"@agent-wechat/cli": patch
---

Add native Desktop voice and video calling backed by `wechat-call-client`, `wechat-audio`, and the bundled `ffmpeg` binary, including Tauri IPC media channels, live camera preview and self-view, bundled `AgentWechatSoftRing.wav` ringtone, macOS camera/microphone entitlements and usage descriptions, and passive OS incoming-call and group-call notification coordination. Also fail closed on macOS when no camera is reported by FFmpeg instead of falling back to AVFoundation index `0` (which can be a screen capture device), and suppress Windows console windows for FFmpeg subprocesses in both Desktop and `wx`.
