# Demo and launch gallery

The approved video and image assets live in [`docs/public/demo`](../../docs/public/demo).
A [GitHub attachment](https://github.com/user-attachments/assets/969f95ef-c17a-4e22-bda0-ec9e08af4ad3) plays inline in the root README.
The video is also [on YouTube](https://www.youtube.com/watch?v=T5LgL4n1MOQ).
This directory preserves the inputs and scripts used to make the README images
and seven Product Hunt cards. It has no production account configuration.

## Rebuild images

From the repository root, with Python 3.10+:

```sh
python3 -m venv work/demo-gallery/venv
. work/demo-gallery/venv/bin/activate
python -m pip install -r scripts/demo-gallery/requirements.txt
python scripts/demo-gallery/render.py
python scripts/demo-gallery/readme_images.py
```

`render.py` writes seven 1270 × 760 PNGs, a review sheet (`preview.jpg`), and
`cards.json` under `docs/public/demo/gallery`. `readme_images.py` writes the README
poster, desktop screenshot, and terminal screenshot under `docs/public/demo`.
Both accept `--output-dir`; the gallery also accepts `--captures-dir`. They only
read local files. Outputs are overwritten; no WeChat messages are sent.

The gallery uses Inter Medium (the font and SIL OFL are in
[`../demo-video/fonts`](../demo-video/fonts)), white text, charcoal `#171819`, and
orange `#bd5721` dividers between the title and body. The archived screenshots
are inputs, so rendering cards does not require a running application or browser.

## What is included

| Asset | Source |
| --- | --- |
| `docs/public/demo/agent-wechat-demo.mp4` | Approved v6 export: 1920 × 1080, 30 fps, 2,776 frames, 92.533 seconds |
| Desktop screenshot | Approved Windows capture, cropped to the app; unrelated conversations already blurred |
| Phone reply crop | Approved Haogang / Nick Bot conversation from the phone recording |
| Terminal crop | Actual `wx` media command from the recording; no fabricated command output |
| Other gallery screenshots | Synthetic Design team, Jin Wu, and Mei Chen conversations in the React client |
| `fixtures/conversations.json` | Human-readable synthetic scene manifest, not an API import format |
| `fixtures/media/afternoon-walk.jpg` | Generated illustration used in the demo; not a photograph documenting the location |
| `fixtures/media/afternoon-walk*.mp4` and compressed JPEG | Exact synthetic media used during capture |
| `fixtures/media/launch-notes.txt` | Synthetic attachment text |

The video includes the approved conversation and voice recordings. Raw phone,
Windows, OpenClaw, and terminal recordings are deliberately excluded. The full
edit recipe, motion cards, and original synthesized instrumental are in
[`../demo-video`](../demo-video/README.md). The final export is included for use
without those private source recordings.

To extract a new approved desktop frame, use FFmpeg on the already privacy-masked
Windows source, then crop away the title bar and taskbar:

```sh
ffmpeg -ss 578 -i /path/to/windows.mp4 -frames:v 1 \
  -vf 'crop=1280:728:0:24' scripts/demo-gallery/captures/desktop-window.png
```

The terminal input is the approved video frame at 79.5 seconds, cropped to
`1792:610:64:140`. The phone reply input is a 1280 × 960 crop at `(0, 1800)` of
the approved phone frame. Review any replacement screenshots for other chats,
notifications, access keys, and private hostnames before publishing them.

## Synthetic demo material

The scene manifest records the synthetic participants, messages, mention, link,
forwarded conversation, and quoted reply used for the gallery. It documents the
visuals; it does not assert live protocol acceptance. The old capture-only mock
server and its deployment configuration are not included.

The original sample encodes are archived in `fixtures/media`. Generate fresh
compressed image and short still-video samples with FFmpeg:

```sh
python scripts/demo-gallery/prepare_media.py
```

Set `FFMPEG` if the executable is not on `PATH`. Outputs go to
`work/demo-gallery/media`, or the supplied `--output-dir`. The generated video is
a four-second still-image clip for testing a media viewer. It is not walking
footage. The archived screenshots retain the media used during the original
capture; different FFmpeg versions/preparation can produce different file sizes.

For an interactive environment, use the project's existing simulator rather than
the archived capture mock:

```sh
pnpm install --frozen-lockfile
pnpm client:dev
```

See the [client preview](../../packages/client-ui/README.md) and
[simulator](../../packages/native-client-rust/crates/wechat-simulator/README.md)
guides. The real hub supports synthetic incoming text/media, group actions, and
send-failure scenarios without connecting to WeChat. It starts its own fixtures;
`conversations.json` is not automatically injected into it.

## Validation status

These are presentation assets for the recorded demo, not an additional capability
guarantee. In particular, a synthetic quoted-reply screenshot does not establish
recipient rendering, and a recorded desktop build does not establish every
platform's installed-app acceptance. Current limits live in
[`FEATURES.md`](../../packages/native-client-rust/FEATURES.md) and
[`PLATFORM-ACCEPTANCE.md`](../../PLATFORM-ACCEPTANCE.md).
