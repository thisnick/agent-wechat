---
"@agent-wechat/cli": patch
---

Keep direct voice calls connected when the legacy WeChat relay audio path reuses a nonce across senders. Derive each sender's starting sequence from room media material and sender identity to reduce early overlap. Continue to reject replay and conflicting packets from the same sender, and stop before the local counter wraps.
