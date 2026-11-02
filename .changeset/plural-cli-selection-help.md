---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

Use `accounts`, `profiles` and `config servers` as the canonical CLI groups and `select` to save account, profile and server choices. Keep previous singular names and selection verbs as hidden compatibility aliases. Alphabetize subcommands at every help level and add command descriptions, selection guidance and examples. Update bundled instructions and repository callers to use the canonical names.
