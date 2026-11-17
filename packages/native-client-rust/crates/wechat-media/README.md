# wechat-media

Image inspection and decoding support shared by `wechat-protocol` and
`wechat-profile`. It identifies supported image formats, checks their dimensions
and handles supported WeChat image containers.

This supporting crate follows the same release version as those libraries.
It does not download attachments, enqueue transfers or manage message delivery.
Some decoding paths need FFmpeg on `PATH`.

`prepare_sticker` validates image/GIF/MOV/MP4 input and produces a GIF plus still
and sampled-frame previews. Fit and trim are caller choices; compliant GIFs retain
their original bytes and loop metadata. `decode_sticker_archive` validates a
selected pack's archive against supplied content identities. Caller-side
`export_sticker_pack` writes numbered GIFs and a checksum index with partial
outcomes and verified resumption. These functions do not establish membership.

See the [Rust libraries reference](https://thisnick.github.io/agent-wechat/reference/rust-libraries/)
for dependencies, compatibility and examples.
