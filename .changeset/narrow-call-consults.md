---
"@agent-wechat/agent-wechat": patch
---

Voice calls consult the OpenClaw agent less often. With the default `consultPolicy: "auto"`, the voice model now answers greetings, small talk and general-knowledge questions itself, and consults for current information, personal records or memory, tools and actions. Previously it was told to report facts only when verified, which led it to consult for almost every answer. `consultPolicy: "substantive"` now also applies to GPT-Live models, worded for their delegation instead of the consult tool.
