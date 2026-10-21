---
"@agent-wechat/agent-wechat": patch
---

Mark whole WeChat threads read after successful inbound handling by default, including commands handled without a model call. The new `native.readPolicy: "on_inbound"` does not require conversation access or model-input confirmation. Retain pending reads across restarts without replaying the message. Ignored messages, pending media and failed handling do not initiate a read. Explicit `never` and `on_model_dispatch` policies remain available.
