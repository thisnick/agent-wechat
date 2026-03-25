---
"@agent-wechat/wechat": patch
---

Add build profiles for new WeChat builds (3eda8254 aarch64, eba86b80 x86_64) with updated chat selection offsets and image XOR masks. Detach Frida hook after selectSession returns to restore function prologue.
