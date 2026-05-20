---
"@agent-wechat/agent-server": patch
---

feat: parse merged-forward (chat history) messages (type 49, subtype 19)

Previously, "Combine and Forward" messages only showed the title (e.g.
"Chat History of Group X"). Now the agent extracts the full
`<recorditem>` XML and renders each forwarded message as
`sender: content`, giving agents visibility into the actual conversation.

Closes #126
