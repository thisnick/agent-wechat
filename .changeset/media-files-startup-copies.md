---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

Stop the daemon from filling the disk with database copies and from keeping every attachment inside each account's `control.sqlite`.

- Starting the daemon no longer copies a database that is already up to date and was closed cleanly. Copies left behind when a start was interrupted (`.sqlite-admission-*` folders) are removed the next time that database opens. The desktop app now waits up to ten minutes for a hub that is still starting instead of stopping it after 30 seconds.
- Images, files, videos and voice notes that were saved into the WeChat profile are read from there; the daemon no longer keeps a second copy. Stickers, link thumbnails and quoted images, which the profile has no place for, are kept as encrypted files in `accounts/<id>/agent/media/`.
- Existing databases are upgraded in the background after start: stored attachments are moved out, files nothing refers to are deleted, and the database file shrinks. An upload is deleted a day after every send that used it has finished; stage the file again to send it later.
- The viewable preview of a WXGF image is converted when requested and is no longer stored.
- If an attachment's file is deleted from the profile or from `agent/media/`, fetching it returns `media_not_cached`. Nothing is downloaded again automatically.
