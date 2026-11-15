---
"@agent-wechat/rust-crates": minor
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
"@agent-wechat/desktop": minor
---

Stickers are no longer limited to 240 pixels. Images and GIFs up to 1024 pixels, the largest sticker WeChat saves, keep their size and encoding, so larger stickers stay sharp on high-resolution screens. A larger file is refused unless you ask to shrink it: `fit` now shrinks to 1024 pixels, and the new `maxEdge` option (`--max-edge` in the CLI) shrinks the longest side to any size from 16 to 1024 pixels. Stickers made from video can set their size with `maxEdge` and their frame rate with the new `fps` option (`--fps`, 1–30); they default to 240 pixels at 10 fps as before. A trim now applies before the 200-frame and 10-second checks, so part of a longer GIF can be kept. Preparing a large animation no longer needs memory for every frame at once.
