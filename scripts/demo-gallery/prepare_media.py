"""Recreate synthetic compressed/photo-video samples from the bundled generated still."""
import argparse
import os
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--output-dir', type=Path, default=ROOT / 'work/demo-gallery/media')
args = parser.parse_args()
output = args.output_dir.resolve()
output.mkdir(parents=True, exist_ok=True)
ffmpeg = os.environ.get('FFMPEG', 'ffmpeg')
source = HERE / 'fixtures/media/afternoon-walk.jpg'


def run(*options):
    subprocess.run([ffmpeg, '-v', 'error', '-y', *map(str, options)], check=True)


run('-i', source, '-frames:v', '1', '-q:v', '12', output / 'afternoon-walk-compressed.jpg')
# A four-second still-image clip for exercising a video viewer, not captured walking footage.
run('-loop', '1', '-i', source, '-t', '4', '-vf',
    'scale=1280:858,format=yuv420p', '-r', '30', '-an', '-c:v', 'libx264',
    '-preset', 'fast', '-crf', '23', '-movflags', '+faststart', output / 'afternoon-walk.mp4')
# Two encodes let a synthetic viewer demonstrate distinct normal/original choices.
run('-i', output / 'afternoon-walk.mp4', '-an', '-c:v', 'libx264',
    '-preset', 'fast', '-crf', '16', '-movflags', '+faststart', output / 'afternoon-walk-original.mp4')
print('Wrote synthetic sample media to', output)
