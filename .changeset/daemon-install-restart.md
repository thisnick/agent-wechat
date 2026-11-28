---
"@agent-wechat/cli": minor
---

Add `wx daemon install`, `wx daemon uninstall`, and `wx daemon restart` to manage a Linux systemd user service or restart a CLI-managed local daemon without changing process ownership rules. On Unix, npm postinstall now maintains a stable native binary symlink under the agent-wechat data directory (`bin/wx`) so systemd units survive package upgrades, and prints a `wx daemon restart` notice when a running local daemon is older than the installed CLI.
