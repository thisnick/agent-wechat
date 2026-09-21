import importlib.machinery
import importlib.util
import pathlib
import unittest

from PIL import Image


MODULE_PATH = pathlib.Path(__file__).with_name("image-download")
LOADER = importlib.machinery.SourceFileLoader("image_download", str(MODULE_PATH))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
image_download = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(image_download)


class ImageMatchTest(unittest.TestCase):
    def setUp(self):
        self.target = Image.new("RGB", (40, 60), "#234b72")
        for x in range(8, 32):
            for y in range(12, 48):
                self.target.putpixel((x, y), (230, 180, 40))

    def test_returns_only_visually_matching_image_row(self):
        screenshot = Image.new("RGB", (800, 600), "#eeeeee")
        screenshot.paste(self.target.resize((80, 120)), (180, 350))
        rows = [
            {"role": "list-item", "name": "Image", "bounds": {"x": 100, "y": 120, "width": 600, "height": 150}},
            {"role": "list-item", "name": "Image", "bounds": {"x": 100, "y": 330, "width": 600, "height": 160}},
        ]

        match = image_download.find_unique_match(screenshot, self.target, rows)

        self.assertEqual(match["row"], rows[1])
        self.assertTrue(180 <= match["x"] <= 260)
        self.assertTrue(350 <= match["y"] <= 470)

    def test_rejects_ambiguous_visual_matches(self):
        screenshot = Image.new("RGB", (800, 600), "#eeeeee")
        screenshot.paste(self.target.resize((80, 120)), (180, 130))
        screenshot.paste(self.target.resize((80, 120)), (180, 350))
        rows = [
            {"role": "list-item", "name": "Image", "bounds": {"x": 100, "y": 110, "width": 600, "height": 150}},
            {"role": "list-item", "name": "Image", "bounds": {"x": 100, "y": 330, "width": 600, "height": 160}},
        ]

        with self.assertRaisesRegex(image_download.DownloadError, "IMAGE_IDENTITY_AMBIGUOUS"):
            image_download.find_unique_match(screenshot, self.target, rows)

    def test_rejects_rows_outside_the_safe_message_viewport(self):
        screenshot = Image.new("RGB", (800, 600), "#eeeeee")
        screenshot.paste(self.target.resize((80, 120)), (180, 10))
        rows = [
            {"role": "list-item", "name": "Image", "bounds": {"x": 100, "y": -5, "width": 600, "height": 150}},
        ]

        with self.assertRaisesRegex(image_download.DownloadError, "IMAGE_CARD_NOT_FOUND"):
            image_download.find_unique_match(screenshot, self.target, rows)


if __name__ == "__main__":
    unittest.main()
