---
name: agent-wechat
description: Operate a configured agent-wechat daemon through the native wx CLI for account status, contact and chat lookup, messaging, media retrieval, and job recovery.
---

# Use the native WeChat CLI

Use this skill for work through `wx`. Preserve the interface and account the user selected; an existing plugin workflow can keep using its own tools.

## Establish context

1. Use the configured native `wx`. Inspect `wx --help` and the relevant subcommand's `--help` when its syntax or availability is uncertain. The installed CLI and `wx --json accounts capabilities` are authoritative.
2. Read `wx --json accounts list` and select the intended account with the per-command `--account ACCOUNT` flag. Resolve names to stable contact/chat IDs before mutations. Ask for clarification only when the request and saved data leave the destination ambiguous.
3. Keep existing server routing and credentials. A remote daemon is normal; a missing local listening port does not show that it is down. Do not create a broader token, replace configuration, or restart services merely to make a read succeed.

For a checkout without `wx` on PATH, the optional [helper](scripts/wx-helper.sh) accepts `AGENT_WECHAT_NATIVE_BINARY` or finds a native build. It preserves argument boundaries and never installs a CLI automatically.

## Perform the requested action

Use `--json` for machine-readable results. Lists contain `items` and may contain `nextCursor`. Preserve IDs and cursors exactly; keep remote message IDs as decimal strings. Results are the returned object, not a universal `outcome/data` wrapper.

For common commands and file locations, read [CLI usage](references/cli-reference.md). Read [runbooks](examples/runbooks.md) for login, recipient resolution, sending, and attachment retrieval. Read only the references needed for the request.

For one send or mutation, keep one idempotency key for that exact intent. Changing the recipient, content, or options is a different intent. Inspect the returned state, not only exit code zero:
- `queued` and `running` are pending.
- A wait ending does not stop daemon work.
- Server acceptance does not prove recipient playback or reading.
- `needs_review` or `unknown` requires inspection of the existing operation; do not resubmit or invent a new key to make it pass.

Use the returned ID with its command: `wx --json jobs get JOB_ID` for media jobs, `wx --json operations get OPERATION_ID` for account/group operations, or `wx --json messages send-status SEND_ID` for text sends. Read [exit codes and recovery](references/exit-codes.md) for failures.

## Files and profiles

Upload source files and download output paths belong to the machine running `wx`. The CLI transfers bytes to/from the daemon. An official-profile path passed to `--official` or `store attach` belongs to the daemon's machine.

Treat CLI execution, a plugin gateway, and the daemon as potentially different filesystems. Use the chosen host's transfer mechanism; a same-named path is not proof that the file exists there.

Official-profile takeover and offline maintenance require verified exclusive ownership of the specific profile. Stop the identified owner normally when authorized, preserve its files, and verify that it stopped. Do not force-kill processes by name, delete locks, or treat `--source-stopped` as evidence by itself.

Disconnect, logout, and forgetting credentials have different effects. Use only the operation requested. For failures, read [troubleshooting](references/troubleshooting.md); the helper's `--doctor` performs only CLI/version and configured-hub status checks.
