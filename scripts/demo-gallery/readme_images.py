"""Build README images from approved captures; does not connect to any service."""
import argparse
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont, ImageOps

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--output-dir', type=Path, default=ROOT / 'docs/public/demo')
args = parser.parse_args()
output = args.output_dir.resolve()
output.mkdir(parents=True, exist_ok=True)
font_path = HERE.parent / 'demo-video/fonts/Inter.ttf'


def font(size):
    result = ImageFont.truetype(str(font_path), size)
    result.set_variation_by_axes([32, 500])
    return result


def load(name):
    with Image.open(HERE / 'captures' / name) as source:
        return source.convert('RGB')


desktop = load('desktop-window.png')
desktop.save(output / 'desktop.jpg', quality=95)
load('terminal-crop.jpg').save(output / 'terminal.jpg', quality=95)

poster = Image.new('RGB', (1600, 900), '#171819')
draw = ImageDraw.Draw(poster)
draw.text((64, 100), 'Agent WeChat', font=font(88), fill='white', anchor='lt')
draw.line((64, 230, 284, 230), fill='#bd5721', width=4)
for index, line in enumerate(('WeChat, your agent,', 'and the terminal.')):
    draw.text((64, 285 + index * 60), line, font=font(42), fill='white', anchor='lt')
draw.text((64, 720), 'Watch the demo · 1:33', font=font(32), fill='white', anchor='lt')
preview = ImageOps.contain(desktop, (850, 650), Image.Resampling.LANCZOS)
x, y = 710 + (850 - preview.width) // 2, (900 - preview.height) // 2
poster.paste(preview, (x, y))
draw.rectangle((x - 1, y - 1, x + preview.width, y + preview.height), outline='#3d3e3f')
poster.save(output / 'poster.jpg', quality=95)
print('Wrote desktop.jpg, terminal.jpg and poster.jpg to', output)
