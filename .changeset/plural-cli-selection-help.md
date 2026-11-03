---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

Use `accounts`, `profiles` and `config servers` as the canonical CLI groups and `select` to save account, profile and server choices. Keep previous singular names and selection verbs as hidden compatibility aliases. Alphabetize subcommands at every help level and add command descriptions, selection guidance and examples. Update bundled instructions and repository callers to use the canonical names.

Use `wx auth logout --local` to sign out on the daemon and keep saved credentials without contacting WeChat. The old `logout-local` command remains available as a hidden alias.

Use `wx chats list` to list chats. The previous `wx threads` command remains available as a hidden compatibility command.
