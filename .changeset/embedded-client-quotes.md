---
"@agent-wechat/client-ui": patch
---

Render received quote excerpts and authors directly from normal message responses,
without the removed quote-resolution endpoint or substituting stored originals.
Use the reply's own media identity for embedded images. Load older local history
only when a quote is clicked, and suppress cached quote content on recall.
