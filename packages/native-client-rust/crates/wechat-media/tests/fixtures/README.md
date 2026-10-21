`static.wxgf` contains a synthetic 32x24 FFmpeg test pattern encoded as one HEVC frame using Debian FFmpeg 5.1.9 and libx265. The source contains no account media. Its minimal WXGF envelope uses the signature, header fields and length-prefixed Annex-B partition layout supported by this decoder. Generated with `testsrc2=size=32x24:rate=1`, `libx265`, `-preset ultrafast`, `pools=none:frame-threads=1:wpp=0`.

`progressive.jpg` is a synthetic image-regression fixture moved from wechat-protocol. `two-frame.webp` is a synthetic two-frame animation fixture.
