---
"@agent-wechat/cli": patch
---

Received videos now download up to 512 MiB, matching the automatic-download setting, instead of stopping at the 50 MiB outgoing video limit. `wx messages media --output` saves videos up to the same size. Downloads whose bytes don't match the message's advertised size now fail as `media_integrity_failed`, as checksum mismatches already did, instead of `media_processing_failed`. For a received video up to 25 MiB, the job also records which sizes and checks differed.
