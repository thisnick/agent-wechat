---
"@agent-wechat/agent-wechat": minor
---

The OpenClaw plugin keeps its place with the event cursor instead of a copy of every message, and catches up on missed messages from the last 24 hours.

- **No message copies.** `consumerStateDir` now holds the event cursor, the identities of messages still being handled, pending thread reads and each group's context boundary. Finished messages and recalls are no longer stored; the daemon commits each message once and applies recalls to the profile. An existing file is converted on first start.
- **Catching up.** After a restart the plugin answers messages that were sent and reached the daemon in the last 24 hours, at most the newest 100 that could start a turn (direct messages, mentions, commands, groups without a mention requirement). Older or excess messages are skipped and logged. A new consumer directory, or a cursor the daemon no longer keeps, resumes at the start of the last 24 hours instead of replaying the event log or stopping. A large backlog no longer fills the queue and stops intake.
- **Group context** for a mention is read from the chat's saved messages, back to the last message handed to the model in that group, with the same sender and recall checks. It can now include messages from before the plugin started, up to `messages.groupChat.historyLimit`; it still leaves out the account's own messages and commands.
- A turn that was in progress when the Gateway stopped is logged at the next start and not repeated, as before.
