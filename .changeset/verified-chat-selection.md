---
"@agent-wechat/agent-wechat": patch
---

Verify that chat selection actually opens the requested chat instead of reporting success when a hook fires. Resolve reordered chats at click time, bound Frida waits, and clean up failed or concurrent selection attempts safely.

Keep repeated selection idempotent, including legacy `--force`, when native identity and the open chat UI agree. This prevents the ARM client from toggling an already-open chat closed and supports chats whose sidebar row is off-screen.
