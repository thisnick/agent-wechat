---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": minor
---

Align incoming voice message downloads and decoding with official WeChat clients to fix intermittent `media_processing_failed` and `media_integrity_failed` errors (#681).

- Support SILK DTX silence frames (`0x0000` via `lostFlag = 1`), `0xFFFF` EOF trailers, headerless `#!SILK_V3` and relaxed leading bytes, and incoming voice notes over 60 seconds in `wechat-protocol::silk`.
- Add `#!OPUS_RAW_V1` decoding (`decode_raw_opus_v1`) in `wechat-calls::opus`, plus AMR/WAV/MP3 FFmpeg fallback for playable WAV conversion.
- Store raw downloaded voice bytes on `--quality encoded` without forcing SILK decode, matching official clients, and add additive CDN voice download support (`VoiceCdnDescriptor` / `CdnClient::download_voice_with_progress`, `filetype = 15`, 16 MiB cap) with fallback to CGI 128 (`downloadvoice`).
- Relax CGI 128 response validation to tolerate server offset/buffer-length drift, shorter transcoded responses (via 3 consecutive empty chunks), and single-chunk overflow past the advertised XML length; handle `cancelflag=1` and streaming `endflag=0 -> 1` in-place updates (`wechat_profile::apply_voice_message_update`); and emit download progress after each chunk.
- Record non-secret `voiceDownloadFailure` diagnostics on failed voice jobs, allow explicit re-download of failed voice assets, and map `media_job_not_retryable` (`409`), `media_queue_full` (`503`), and `voice_cancelled` / `media_cancelled` (`410`) instead of `500 operation_failed`.
