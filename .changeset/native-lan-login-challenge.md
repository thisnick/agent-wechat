---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Add experimental handling for the server-directed LAN challenge during desktop phone/QR login. Advertise local IPv4 addresses through the native login service and answer the phone's temporary TCP token exchange, with cancellation, expiry and one upload per attempt. Live challenge acceptance remains unverified.
