---
"@agent-wechat/desktop": patch
"@agent-wechat/client-ui": patch
---

Fix the voice note player in Safari and the macOS app, where the overflow (»)
button could be drawn on top of the play button. WebKit doesn't always lay out
its native media controls again after the media loads or after the page returns
from the background, so the voice player now makes WebKit redo that layout. Removes
the font override from the previous attempt, which had no visible effect.
