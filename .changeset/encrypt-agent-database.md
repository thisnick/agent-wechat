---
"@agent-wechat/cli": minor
---

Encrypt agent.db at rest using SQLCipher with the auth token as the encryption key. Existing unencrypted databases are automatically migrated on startup. If decryption fails (e.g. token changed), the database is discarded and recreated fresh.
