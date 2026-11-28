---
"@agent-wechat/cli": minor
---

Add `wx feedback [--open]` to print (or open in a browser) the repository link and a pre-filled GitHub bug-report URL with non-sensitive version, install-method and platform details. Add a one-time GitHub star notice on stderr after accepted sends across 3 local days and 15 sends (stored in `support-prompt.json` next to `client.toml`, suppressed by `AGENT_WECHAT_NO_PROMPTS=1`, `CI`, or piped non-JSON output), and include `"report": "wx feedback --json"` (and a human-mode hint) on unclassified exit-1 errors.
