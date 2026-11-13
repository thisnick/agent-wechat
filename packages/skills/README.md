# Agent skills

The [agent-wechat skill](agent-wechat/SKILL.md) teaches agents to use a configured native `wx` CLI: resolve recipients, send requested content once, retrieve media, and inspect existing operations.

Copy `packages/skills/agent-wechat` from the upstream [agent-wechat repository](https://github.com/thisnick/agent-wechat/tree/main/packages/skills/agent-wechat) to the skill directory supported by your agent. Keep the whole directory so its relative references and optional Bash helpers work.

The skill does not install or configure a daemon. The installed CLI's `--help` and account capabilities describe the available operations.

Run the package's offline checks:

```sh
bash packages/skills/agent-wechat/scripts/test-skill.sh
```

The checks use fake CLI processes and temporary files. They do not require a live account, global skill installation, or a running daemon.
