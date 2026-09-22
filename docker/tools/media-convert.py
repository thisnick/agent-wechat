#!/usr/bin/env python3
"""
Convert WeChat proprietary media formats to standard formats.

Modes:
  wxgf2img  — WXGF (HEVC in custom container) → JPEG or GIF
  silk2mp3  — SILK_V3 voice audio → MP3

Usage:
  media-convert wxgf2img < input.wxgf > output.jpg
  media-convert silk2mp3 < input.silk > output.mp3

Reads binary from stdin, writes converted binary to stdout.
Format hint printed to stderr as "FORMAT:<ext>".
"""

import sys
import os
import struct
import subprocess
import tempfile

MIN_RATIO = 0.6


def validate_image(data):
    """Reject incomplete cache writes before decoding with the runtime codec."""
    if data.startswith(b'\x89PNG\r\n\x1a\n'):
        complete = data.endswith(b'\x00\x00\x00\x00IEND\xaeB`\x82')
    elif data.startswith(b'\xff\xd8'):
        complete = data.endswith(b'\xff\xd9')
    elif data.startswith((b'GIF87a', b'GIF89a')):
        complete = data.endswith(b';')
    elif data[:4] == b'RIFF' and data[8:12] == b'WEBP':
        complete = len(data) >= 12 and struct.unpack('<I', data[4:8])[0] + 8 == len(data)
    else:
        complete = False
    if not complete:
        raise ValueError('Incomplete image')
    result = subprocess.run(
        ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-xerror',
         '-err_detect', 'explode', '-i', 'pipe:0', '-f', 'null', '-'],
        input=data, capture_output=True, timeout=10,
    )
    if result.returncode or result.stderr:
        raise ValueError('Image validation failed')
    return b'ok', 'validation'

# ============================================
# WXGF → Image
# ============================================

def find_data_partitions(data):
    """Find HEVC NALU partitions in WXGF data.

    Port of chatlog's wxgf.go findDataPartition().
    Scans for HEVC start codes (0x00000001 or 0x000001) with 4-byte BE
    length prefix before each start code.
    """
    if len(data) < 15 or data[0:4] != b"wxgf":
        raise ValueError("Invalid WXGF data")

    header_len = data[4]
    if header_len >= len(data):
        raise ValueError("Invalid WXGF header length")

    patterns = [b"\x00\x00\x00\x01", b"\x00\x00\x01"]

    for pattern in patterns:
        partitions = []
        offset = 0

        while header_len + offset < len(data):
            idx = data.find(pattern, header_len + offset)
            if idx == -1:
                break

            if idx < 4:
                offset = idx - header_len + 1
                continue

            length = (data[idx - 4] << 24 | data[idx - 3] << 16 |
                      data[idx - 2] << 8 | data[idx - 1])

            if length <= 0 or idx + length > len(data):
                offset = idx - header_len + 1
                continue

            ratio = length / len(data)
            partitions.append({
                "offset": idx,
                "size": length,
                "ratio": ratio,
            })
            offset = idx - header_len + length

        if partitions:
            max_idx = max(range(len(partitions)), key=lambda i: partitions[i]["ratio"])
            max_ratio = partitions[max_idx]["ratio"]
            return partitions, max_idx, max_ratio

    raise ValueError("No HEVC partitions found in WXGF data")


def like_anime(partitions, max_ratio):
    """Detect if WXGF contains animation (alternating anime/mask frames)."""
    return len(partitions) > 1 and max_ratio < MIN_RATIO


def convert_wxgf_static(hevc_data):
    """Convert a single HEVC frame to JPEG via ffmpeg."""
    proc = subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "error",
         "-i", "-",
         "-vframes", "1",
         "-c:v", "mjpeg",
         "-q:v", "4",
         "-f", "image2",
         "-"],
        input=hevc_data,
        capture_output=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"ffmpeg failed: {proc.stderr.decode()}")
    if not proc.stdout:
        raise RuntimeError("ffmpeg produced no output")
    return proc.stdout, "jpeg"


def convert_wxgf_animated(data, partitions):
    """Convert alternating anime/mask HEVC frames to GIF via ffmpeg."""
    anime_frames = []
    mask_frames = []
    for i, p in enumerate(partitions):
        chunk = data[p["offset"]:p["offset"] + p["size"]]
        if i % 2 == 0:
            mask_frames.append(chunk)
        else:
            anime_frames.append(chunk)

    # Write to temp files (ffmpeg needs seekable input for multi-input)
    with tempfile.NamedTemporaryFile(suffix=".hevc", delete=False) as af:
        for frame in anime_frames:
            af.write(frame)
        anime_path = af.name

    with tempfile.NamedTemporaryFile(suffix=".hevc", delete=False) as mf:
        for frame in mask_frames:
            mf.write(frame)
        mask_path = mf.name

    try:
        proc = subprocess.run(
            ["ffmpeg", "-hide_banner", "-loglevel", "error",
             "-i", anime_path,
             "-i", mask_path,
             "-filter_complex",
             "[0:v][1:v]alphamerge,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse",
             "-f", "gif",
             "-"],
            capture_output=True,
        )
        if proc.returncode != 0:
            raise RuntimeError(f"ffmpeg animated failed: {proc.stderr.decode()}")
        if not proc.stdout:
            raise RuntimeError("ffmpeg animated produced no output")
        return proc.stdout, "gif"
    finally:
        os.unlink(anime_path)
        os.unlink(mask_path)


def wxgf2img(data):
    """Convert WXGF data to JPEG or GIF."""
    partitions, max_idx, max_ratio = find_data_partitions(data)

    if like_anime(partitions, max_ratio):
        return convert_wxgf_animated(data, partitions)
    else:
        p = partitions[max_idx]
        hevc_data = data[p["offset"]:p["offset"] + p["size"]]
        return convert_wxgf_static(hevc_data)


# ============================================
# SILK → MP3
# ============================================

def silk2mp3(data):
    """Convert SILK_V3 voice data to MP3.

    WeChat SILK variant: prepends 0x02 before standard #!SILK_V3 header.
    Uses pysilk (silk-python) for SILK→PCM, then ffmpeg for PCM→MP3.
    """
    import io
    import pysilk

    # Strip WeChat's 0x02 prefix if present (non-standard SILK header)
    if data and data[0:1] == b"\x02":
        data = data[1:]

    silk_io = io.BytesIO(data)
    pcm_io = io.BytesIO()

    # Decode SILK → PCM (24kHz mono s16le)
    pysilk.decode(silk_io, pcm_io, 24000)
    pcm_data = pcm_io.getvalue()

    proc = subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "error",
         "-f", "s16le",
         "-ar", "24000",
         "-ac", "1",
         "-i", "-",
         "-c:a", "libmp3lame",
         "-q:a", "2",
         "-f", "mp3",
         "-"],
        input=pcm_data,
        capture_output=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"ffmpeg failed: {proc.stderr.decode()}")
    if not proc.stdout:
        raise RuntimeError("ffmpeg produced no output")
    return proc.stdout, "mp3"


# ============================================
# Main
# ============================================

def main():
    if len(sys.argv) < 2:
        print("Usage: media-convert <wxgf2img|silk2mp3>", file=sys.stderr)
        sys.exit(1)

    mode = sys.argv[1]
    data = sys.stdin.buffer.read()

    if not data:
        print("Error: no input data", file=sys.stderr)
        sys.exit(1)

    try:
        if mode == "validate-image":
            result, fmt = validate_image(data)
        elif mode == "wxgf2img":
            result, fmt = wxgf2img(data)
        elif mode == "silk2mp3":
            result, fmt = silk2mp3(data)
        else:
            print(f"Unknown mode: {mode}", file=sys.stderr)
            sys.exit(1)

        print(f"FORMAT:{fmt}", file=sys.stderr)
        sys.stdout.buffer.write(result)

    except Exception as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
