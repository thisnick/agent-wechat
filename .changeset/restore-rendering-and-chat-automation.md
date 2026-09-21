---
"@agent-wechat/agent-wechat": patch
"@agent-wechat/agent-server": patch
---

Fix frozen container rendering by running Xvfb and x11vnc as the WeChat user, while preserving read-only VNC. Store proxy configuration in a private runtime directory so proxied containers can restart reliably.

Verify that chat selection opens the requested chat before reporting success, resolve reordered chats at click time, and keep repeated ARM chat selection from closing the active chat.

Support both Send and Send(S) buttons and the nested message composer layout in newer WeChat builds.
