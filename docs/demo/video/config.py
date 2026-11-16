"""Local media and output locations; no account or infrastructure configuration."""
import os
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
MEDIA = Path(os.environ.get("DEMO_MEDIA_DIR", ROOT / "work/demo-video/input")).resolve()
OUTPUT = Path(os.environ.get("DEMO_OUTPUT_DIR", ROOT / "work/demo-video/render")).resolve()
FFMPEG = os.environ.get("FFMPEG", "ffmpeg")
OUTPUT.mkdir(parents=True, exist_ok=True)
