---
"@agent-wechat/client-ui": patch
"@agent-wechat/desktop": patch
---

The app no longer signs in again on its own after WeChat signs an account out. When that happens while the account's chats are open, the sign-in screen now waits with **Sign in** and **Sign in with a QR code instead**. Before, it started a sign-in at once, which on a trusted computer can finish without the phone, even if you had just signed in somewhere else on purpose. Opening the account from the account list still signs in right away.

The sign-in screen also no longer stops on "This profile has an operation in progress" right after a sign-in succeeds. The hub refuses a new sign-in for a few seconds while the account's first sync runs. The screen now says the account is still finishing other work, tries again on its own, and opens the chats as soon as the account is signed in.
