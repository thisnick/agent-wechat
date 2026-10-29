# Synthetic video source

`legacy-mjpeg.mov` is a one-second, 160×120 MJPEG test pattern with no audio or user content. It exercises older QuickTime movies that do not contain an `ftyp` atom.

Generated with FFmpeg 5.1.9:

```bash
ffmpeg -f lavfi -i testsrc2=size=160x120:rate=5:duration=1 \
  -c:v mjpeg -threads 1 -f mov source.mov
```

The first 20-byte `ftyp` atom was removed and every `stco` sample offset under `moov/trak/mdia/minf/stbl` reduced by 20 bytes. The `legacy_quicktime` test helper in `src/media_video.rs` performs this adjustment on a generated fixture too. FFprobe reads the retained fixture as a one-second MJPEG movie.

- Size: 33,558 bytes
- SHA-256: `a5bfa72b32041a99cb56f8a004b66b3ce47dc55eba9bc5e3560b1df8dc2b1002`
