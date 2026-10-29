---
"@agent-wechat/client-ui": patch
"@agent-wechat/desktop": patch
---

Stage attached, pasted and dropped files in the message composer with previews and remove controls. The composer's Send action queues text and each selected file as separate messages, retaining each chat's unsent attachments until sent or removed.

Use the hub's per-type source limits for larger attachments and display local validation errors accurately. Upload browser File/Blob sources without first copying the whole file into memory. Show optional keyboard and original media quality guidance on demand.
