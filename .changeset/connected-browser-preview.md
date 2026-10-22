---
"@agent-wechat/client-ui": minor
"@agent-wechat/shared": patch
---

Add a connected browser development client for the isolated Rust hub and synthetic upstream, with QR sign-in, account selection, conversations, text send receipts and event replay. Render valid synthetic QR codes and renew expired codes automatically using the hub login policy. Show the refresh countdown and separate simulator test controls from sign-in. Scale the sign-in illustration proportionally from the text's left edge, use one search focus ring and center unread counts. Search stored conversation history through the hub with paginated matches and discard late results after switching conversations. Preserve the browser fetch receiver in the shared SDK and expose stream activity so clients report a connection after receiving a frame.

Resume QR login and phone approval after a page reload using the same saved operation and admission key.
