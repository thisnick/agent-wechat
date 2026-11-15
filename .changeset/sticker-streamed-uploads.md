---
"@agent-wechat/rust-crates": minor
"@agent-wechat/desktop": minor
---

Stickers can now be made from videos up to 512 MB; images and GIFs stay limited to 64 MB. Uploads are written to disk as they arrive, and sticker preparation reads the video from a temporary file, so a large source no longer needs its whole size in the daemon's memory. **Add to Saved** deletes each upload once its sticker is added, or when the dialog closes, and says separately whether a file is empty or too large. API clients can do the same with the new `DELETE /uploads/{id}`, which returns 409 `upload_in_use` while a send or preparation still needs the upload. The daemon now also deletes finished uploads every hour instead of only at startup. The OpenClaw sticker tool keeps its 64 MB limit, because OpenClaw's workspace bridge reads a whole file into memory.
