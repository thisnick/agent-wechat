import importlib.machinery
import importlib.util
import pathlib
import unittest
from types import SimpleNamespace
from unittest import mock


MODULE_PATH = pathlib.Path(__file__).with_name("file-download")
LOADER = importlib.machinery.SourceFileLoader("file_download", str(MODULE_PATH))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
file_download = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(file_download)


class WeixinGeometryTest(unittest.TestCase):
    def test_uses_largest_visible_weixin_window(self):
        geometries = {
            "2": "X=359\nY=179\nWIDTH=560\nHEIGHT=440\n",
            "1": "X=200\nY=80\nWIDTH=880\nHEIGHT=640\n",
        }

        def command(*args, **_kwargs):
            if args[1] == "search":
                return SimpleNamespace(stdout="1\n2\n", returncode=0)
            if args[1] == "getwindowgeometry":
                return SimpleNamespace(stdout=geometries[args[-1]], returncode=0)
            if args[1] == "getactivewindow":
                return SimpleNamespace(stdout="1\n", returncode=0)
            return SimpleNamespace(stdout="", returncode=0)

        with mock.patch.object(file_download, "command", side_effect=command):
            geometry = file_download.weixin_geometry()

        self.assertEqual(geometry, {"X": 200, "Y": 80, "WIDTH": 880, "HEIGHT": 640})

    def test_find_row_tolerates_delayed_a11y_refresh(self):
        empty_tree = {"children": []}
        target = {
            "role": "list-item",
            "name": "File\n报告.docx\n12K\n微信电脑版",
            "bounds": {"x": 501, "y": 200, "width": 578, "height": 120},
        }
        with (
            mock.patch.object(
                file_download,
                "tree",
                side_effect=[empty_tree, empty_tree, empty_tree, empty_tree, target],
            ),
            mock.patch.object(file_download, "scroll"),
        ):
            row = file_download.find_row(
                "报告.docx", {"X": 200, "Y": 80, "WIDTH": 880, "HEIGHT": 640}
            )

        self.assertEqual(row, target)


if __name__ == "__main__":
    unittest.main()
