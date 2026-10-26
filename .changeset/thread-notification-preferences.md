---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/shared": patch
"@agent-wechat/desktop": patch
"@agent-wechat/client-ui": patch
---

Expose saved direct-chat and group notification preferences as nullable `muted` values on thread lists and detail, using batched local reads per page. Receive phone contact changes through the independent contact sync stream, retain unsupported controls, and preserve official cursor publication and restart fences.

Add native desktop notifications for new incoming messages in known unmuted chats, including when the window is hidden. Suppress history, outgoing messages, duplicates, stale mute state and the currently focused conversation. Add a settings toggle and click-to-open navigation.

Support direct-chat mute/unmute through the existing thread notification API and CLI alongside group controls. Add signed desktop update discovery, channel settings, verified downloads, graceful install/restart, and final-artifact update manifests in the release workflow.
