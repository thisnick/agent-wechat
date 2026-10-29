---
"@agent-wechat/cli": patch
---

Allow history and cached media reads to run alongside account updates using request-owned WAL snapshots. Separate incoming download limits from the send queue, release the account writer during download and sync network waits, and preserve login, cancellation and local repair checks before committing results. Keep serialized reads for other journal modes and expose read timings in HTTP response headers.
