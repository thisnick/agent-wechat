---
"@agent-wechat/agent-wechat": patch
---

The owner-only `wechat_contacts` and `wechat_contact_request` tools now appear for agents that run through a CLI backend such as Claude CLI. OpenClaw passes those runs no memory partition, so the tools were never offered, even in the owner's private chat. When a run has no memory partition, the plugin now accepts the owner's own direct chat session instead, for the per-person `session.dmScope` settings. Groups, channels, scheduled runs, sub-agents and the shared `main` session still don't get them, and a memory partition that OpenClaw does supply still decides.
