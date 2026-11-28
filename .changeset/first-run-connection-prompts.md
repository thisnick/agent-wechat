---
"@agent-wechat/cli": minor
---

Classify local and remote connection failures across `wx` commands (distinguishing `daemon_not_running`, desktop-hub or daemon key mismatches, and remote `endpoint_unavailable`), add structured `next` command hints to JSON and human errors, add interactive first-run prompts to start the local daemon or choose/create an account, add a `client` section to `wx status`, lead `wx --help` with the local first-run flow, and print human-mode next steps after a successful `wx auth login`.
