# Synthetic large JPEGs

The Zstandard files contain baseline and progressive JPEGs generated with Pillow,
with no camera metadata or personal content. Both are 6,128 × 8,160 RGB images:
blue `(32, 128, 224)` with a red `(224, 64, 32)` lower-right quadrant. They exercise
50-megapixel decoding without storing a large fixture in Git. The grayscale
baseline fixture has the same dimensions and uniform luminance 128.

To regenerate, create an RGB image of that size, draw the quadrant starting at
`(3064, 4080)`, save JPEG with `quality=85`, `optimize=True`, and respectively
`progressive=False` / `True`, then compress each JPEG with `zstd -19`.
