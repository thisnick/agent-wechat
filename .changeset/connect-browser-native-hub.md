---
"@agent-wechat/client-ui": minor
---

Add a separate browser launcher for an explicitly supplied native hub, account and private token file. Keep simulator startup independent, proxy only the selected account and status/account-list reads, protect bootstrap with a private secret and origin checks, and leave the external daemon running when the browser launcher closes.
