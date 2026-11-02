---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

Include local microphone/speaker support in released executables, add local audio-device discovery, and show live signal-level meters during interactive calls. Keep passive/JSON output free of terminal meters and retain separate daemon calling permissions and native transmission guards. Linux executables now require libpulse0; daemon operation does not require audio devices or a sound server.
