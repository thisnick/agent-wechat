---
"@agent-wechat/agent-wechat": minor
---

Remove the separate contact privacy read route and `wx contacts privacy get`. Read `privacy` and `privacyVersion` from the contact instead (`wx contacts get`, `GET contacts/{id}`), which now include them. Pass `privacyVersion` as `expectedVersion` when changing privacy.
