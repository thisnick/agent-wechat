---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/client-ui": patch
"@agent-wechat/rust-crates": patch
---

Fix adding larger custom stickers by splitting encrypted upload requests across transport records. Preserve collection revision headers in the desktop app so moving and removing Saved stickers reaches the daemon with the required precondition.

Make Add to Saved use one primary Add button, show saving progress, and return to Saved after successful imports. Keep partial failures open for retry without resubmitting completed items.

Simplify import review to previews and file selection by removing fit, start and duration controls.

Accept verified complete-upload acknowledgements when adding previously uploaded stickers again. Show sticker job failures separately from connection errors, and show the dragged sticker and its drop position while reordering Saved. Keep edit controls still instead of jiggling.
