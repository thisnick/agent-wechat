# Demo video source

This is the edit recipe for the approved October 28, 2026 demo: phone →
OpenClaw → phone voice note → recorded Windows client → terminal → saved image.
It preserves the recorded demo's copy and interfaces, including the historical
Windows client; it is not a compatibility statement for current releases.

The final cut is **1920 × 1080, 30 fps, 2,776 frames (92.533 seconds)**.
The approved final export is included at
[`docs/public/demo/agent-wechat-demo.mp4`](../../docs/public/demo/agent-wechat-demo.mp4).
Private raw recordings, standalone voice notes, credentials, and intermediate
renders are not stored in Git. Keep the original recordings separately to rebuild
the full video. The title preview and instrumental can be generated without them.
README and gallery images have their own [rebuild guide](../demo-gallery/README.md).

## Run

Requires Python 3.10+, FFmpeg with H.264/AAC/MP3 encoders, and the Python packages
below. Run from the repository root:

```sh
python3 -m venv work/demo-video/venv
. work/demo-video/venv/bin/activate
python -m pip install -r scripts/demo-video/requirements.txt

# Independent previews; no recordings required.
python scripts/demo-video/titles.py --frames
python scripts/demo-video/titles.py
python scripts/demo-video/music.py

# Full edit, with the local inputs listed below.
python scripts/demo-video/render.py
python scripts/demo-video/finish.py
```

Outputs go to `work/demo-video/render/`. The final file is
`agent-wechat-demo.mp4`; `timeline.json` and `manifest.json` accompany it.
`demo-background-music.mp3` is the standalone instrumental without dialogue
volume changes. `motion-preview.mp4` previews the titles and closing card.

Set `FFMPEG` to an executable path if it is not on `PATH`. `DEMO_MEDIA_DIR` and
`DEMO_OUTPUT_DIR` override the input and output directories. Render commands
replace outputs in that directory. They do not contact a daemon or send messages.

## Local inputs

Place these files in `work/demo-video/input/`. Timings and crop coordinates are
specific to the original takes; replacement footage needs a new edit.

| Filename | Recording |
| --- | --- |
| `phone-message.mp4` | First phone take, 1280 × 2856; question typed and sent |
| `phone-voice.mp4` | Second phone take, 1280 × 2856; reply, recording gesture and actual send animation |
| `openclaw.webm` | OpenClaw capture; selected segment starts at 107 s, 537 × 650 |
| `windows.mp4` | Windows capture, 1280 × 800, with unrelated conversation list already blurred |
| `terminal.webm` | Terminal capture, 1920 × 1080; real command and downloaded-file result |
| `human-voice.wav` | Recovered outgoing voice note, 8.180 s |
| `agent-voice.wav` | Recovered agent reply, 15.960 s |
| `walk.jpg` | The same image downloaded through the CLI, 640 × 429 |

The sent-message still is extracted from the first phone take at 13 seconds.
The Windows source must already obscure unrelated chat heads and previews.
Additional notification masks in `render.py` cover only the observed toast
intervals in these takes. Review any replacement footage for other chats,
notifications and credentials before exporting it.

## Edit and motion direction

- One surface at a time, with full-screen text cards between narrative beats.
- Inter Medium, optical size 32; white text on `#171819`, orange `#BD5721`.
- Text reveals by phrase. Underlines draw once, then hold; no decorative loops.
- Match the outgoing message to the receiving surface before zooming out.
- OpenClaw reply zoom begins at 22 seconds and holds for reading.
- Keep the phone recording gesture, upload and settled voice bubble continuous.
- Windows and terminal typing play at 1.5×; both voice notes stay at original speed.
- Show image arrival for 3.5 seconds, then the terminal title.
- Dissolve from the terminal result into the downloaded image over 0.7 seconds.
- Closing card is left aligned with only two text sizes: 120 and 42 pixels.

The approved copy and underline targets live in `titles.py`. The final timing is
saved in `approved-timeline.json`. `render.py` creates a 94.033-second intermediate
cut; `finish.py` removes its final 45 frames of the image-in-chat shot and mixes
music, preserving all subsequent audio/video alignment.

## Audio

`music.py` synthesizes an original 96 BPM instrumental in D major using NumPy:
soft kick, filtered hats, rim, sine bass, FM electric-piano-like chords and a quiet
pad. It uses no sampled recordings. A fixed random seed makes the synthesis
repeatable. FFmpeg levels the bed to approximately −26 LUFS; the bed lowers by
10.46 dB during the voice notes (33.0–41.18 s and 49.8–65.76 s), with gradual
attack and release. Speech remains at the level used in the approved base edit.

## Verify an export

```sh
ffprobe -v error -show_entries format=duration:stream=codec_name,width,height,r_frame_rate,nb_frames \
  -of json work/demo-video/render/agent-wechat-demo.mp4
ffmpeg -v error -i work/demo-video/render/agent-wechat-demo.mp4 -f null -
```

Check the send animation, voice alignment, image arrival, terminal command and
terminal-to-image dissolve visually. The committed timeline contains filenames
rather than local account paths. The bundled, unmodified Inter font is licensed
under the SIL Open Font License; see `fonts/OFL.txt`.

## GitHub inline copy

The README uses the [video attachment on issue #324](https://github.com/thisnick/agent-wechat/issues/324#issuecomment-6480211337).
Its 9,545,885-byte MP4 fits GitHub's
[10 MB free-plan video limit](https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/attaching-files). The H.264 video
stream is unchanged; only the AAC audio is re-encoded from 256 to 128 kbps. The
full-quality export remains under `docs/public/demo`.

Recreate the smaller attachment from the repository root:

```sh
mkdir -p work/demo-gallery
ffmpeg -i docs/public/demo/agent-wechat-demo.mp4 \
  -map 0:v:0 -map 0:a:0 -c:v copy -c:a aac -b:a 128k \
  -movflags +faststart work/demo-gallery/agent-wechat-demo-github.mp4
```

Upload that file through a GitHub issue/comment attachment picker and put the
returned `https://github.com/user-attachments/assets/...` URL on its own line in
the README. Use that stable attachment URL, not an expiring media-delivery URL.
