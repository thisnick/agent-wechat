---
"@agent-wechat/agent-wechat": patch
---

Stop warning forever about OpenClaw messages held after an uncertain reply. The plugin never retries a message whose model dispatch or reply it can't confirm, because resending could reply twice. It used to keep each one, plus answered messages whose phone read receipt came back uncertain, as a permanent "needs review" warning in `openclaw channels status` and never pruned them. It now logs a warning when it holds a message, and prunes held messages with the rest of the finished ones.
