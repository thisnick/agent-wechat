---
"@agent-wechat/agent-wechat": patch
---

The owner-only `wechat_contacts` and `wechat_contact_request` tools now work for agents that run through a CLI backend such as Claude CLI. OpenClaw doesn't tell tools in those runs whether the chat is private, so the tools were never offered, even in the owner's own chat. When OpenClaw supplies no such answer, the plugin now checks the stored session itself: it must be the current direct chat, keyed by and replying to the owner. Groups, scheduled runs, sub-agents, other people's chats and the shared `main` direct-chat session still don't get the tools, and every call checks the session again before it acts and before it returns. When OpenClaw does supply an answer, that answer still decides.
