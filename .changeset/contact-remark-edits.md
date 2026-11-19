---
"@agent-wechat/rust-crates": minor
"@agent-wechat/agent-wechat": minor
---

You can now set or clear your private name for a contact (its remark): `wx contacts set CONTACT_ID --remark "Name"` or `--clear-remark`, or `PATCH contacts/CONTACT_ID` with `{"remark": "Name", "expectedVersion": "…"}` (`null` clears it). The name shows on your phone, and as the contact's `displayName`, a few seconds after WeChat accepts it; until then the contact reports `remarkPending: true`. Change privacy and the remark in separate requests. Friends whose privacy can't be edited yet report why in `privacyUnavailable`. The protocol crate adds `contact_remark_request`.
