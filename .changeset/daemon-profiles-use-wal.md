---
"@agent-wechat/agent-wechat": patch
---

New profiles the daemon creates now keep their databases in WAL mode, as the official client does, and the daemon converts existing profiles it created from the rollback journal to WAL at startup. Reads on these profiles no longer wait behind sync writes (#537). Attached official profiles are never converted.
