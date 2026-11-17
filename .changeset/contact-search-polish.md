---
"@agent-wechat/client-ui": patch
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
"@agent-wechat/shared": patch
---

Show contact, group and message search results as each source responds, default Contacts search to its contact scope, and simplify the shared contact card with icon details actions, collapsed history and a single contextual footer.

Report stored contact membership evidence in the existing relation field. Keep unknown people out of the saved-contact list and offer a separate lookup and friend-request review from their contact cards.

Contact lists now default to confirmed saved friends across the API and CLI. Use the API relation filter or `wx contacts list --all` for other cached people. Exact reads and server discovery are unchanged.

Local contact search defaults to saved friends. When a query is entered, show other cached people in a separate Other people section. Show server discovery separately under Add someone on WeChat, label friends Contact and confirmed non-contacts Not friends yet, and omit a relationship label when membership is unknown.

Separate Friends and Other people search tabs, route Contacts + to Other people, page each section independently, and show original dates and times on message matches.
