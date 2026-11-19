---
"@agent-wechat/client-ui": patch
"@agent-wechat/desktop": patch
---

The sign-in screen no longer stops on "This profile has an operation in progress" right after a sign-in succeeds. The hub refuses a new sign-in for a few seconds while the account's first sync runs. The screen now says the account is still finishing other work and tries again on its own, and opens the chats as soon as the account is signed in. Before, it showed the error and you had to wait and press **Try again**.
