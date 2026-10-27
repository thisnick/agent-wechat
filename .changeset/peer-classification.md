---
"@agent-wechat/cli": minor
"@agent-wechat/shared": minor
---

Add profile-derived peer classification to contact, thread, group and member reads, including official/business account kinds and cached multilingual OpenIM labels. Keep conversation kind and relationship separate, with unknown values preserved. Incoming ordinary business metadata updates both the official business row and contact extra data in one transaction. Existing schema, frontend and routing policies are unchanged; separate OpenIM synchronization remains unsupported.
