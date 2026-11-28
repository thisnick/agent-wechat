---
"@agent-wechat/cli": patch
---

Cache native binary verification in an atomic metadata stamp written at install time so the npm `wx` launcher checks file metadata (`size`, `mtime`, `ctime`, `inode`, `device`), manifest SHA-256, and cached identity instead of re-hashing the binary and running `wx --json version` on every command. Any missing, invalid, or mismatched stamp falls back to full verification.
