`static.wxgf` contains a synthetic 32x24 FFmpeg test pattern encoded as one HEVC frame using Debian FFmpeg 5.1.9 and libx265. The source contains no account media. Its minimal WXGF envelope uses the signature, header fields and length-prefixed Annex-B partition layout supported by this decoder. Generated with `testsrc2=size=32x24:rate=1`, `libx265`, `-preset ultrafast`, `pools=none:frame-threads=1:wpp=0`.

`progressive.jpg` is a synthetic image-regression fixture moved from wechat-protocol. `two-frame.webp` is a synthetic two-frame animation fixture.

`channels-prefix-vectors.json` holds synthetic WeChat Channels prefix keystream vectors: for seeds 0, 1, 42, 2^53+1 and 2^64-1, the first 64 bytes, bytes 2040 to 2063 (across the first 2048-byte batch) and the SHA-256 of the full 131,072-byte stream. The seeds are synthetic; no account or media data is involved.

`sticker-pack.zip` is a synthetic sticker-pack archive for the `pack.fixture` product: an icon, two numbered 1x1 GIF stickers under `large/` and their PNG thumbnails under `thumb/` (43-69 bytes each). It contains no account or downloaded pack data.
