---
"@agent-wechat/cli": minor
---

Add `wx daemon doctor` for read-only local daemon diagnostics (`--all`, `--fix`, `--json`), checking SQLite WAL/SHM/journal shapes via the startup admission checks, schema versions, free space and filesystem type, ownership and permissions, stale `process.json` records, port occupancy, `--official` profile writeability and overlaps, `daemon.log` errors, FFmpeg/FFprobe availability, and live account/storage diagnostics via `GET /v1/accounts/{a}/storage`. Include the last `daemon.log` error and a pointer to `wx daemon doctor` when `wx daemon start`, `wx daemon status`, or `wx serve` fails.
