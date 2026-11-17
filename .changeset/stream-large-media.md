---
"@agent-wechat/rust-crates": minor
"@agent-wechat/agent-wechat": patch
---

Sending, downloading and serving large files and original-quality videos no longer loads them into the daemon's memory (#529). A send reads its upload from a private temporary file and encrypts each CDN chunk as it goes; a download decrypts each piece into a temporary file; both write the official profile and the daemon's encrypted copy from that file. `GET /media/{id}/content` streams the file, and a `Range` request reads only its range once the whole file was checked. A 512 MiB send or download now stays within a small, fixed amount of memory instead of needing several hundred MB.

`wechat-protocol` adds `PreparedFileUpload` with `CdnClient::upload_file_with_progress`, `upload_large_file_from_file_with_progress` and `upload_video_original_from_file_with_progress`, plus `download_to_with_progress`, `DownloadReceipt` and `DownloadedMedia::into_parts` for downloads into a writer. `wechat-profile` adds `MediaPayload`, `cache_file_projection_payload`, `cache_video_projection_payload`, `publish_cache_payload` and `ProfileReader::verified_hardlink_file`. Existing APIs are unchanged.
