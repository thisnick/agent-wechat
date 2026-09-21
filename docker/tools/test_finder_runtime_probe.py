import importlib.machinery
import importlib.util
import json
import pathlib
import tempfile
import unittest
from unittest import mock


MODULE_PATH = pathlib.Path(__file__).with_name("finder-runtime-probe")
LOADER = importlib.machinery.SourceFileLoader("finder_runtime_probe", str(MODULE_PATH))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
finder_runtime_probe = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(finder_runtime_probe)


class FinderRuntimeProbeTest(unittest.TestCase):
    def test_reports_exact_json_endpoint_without_exposing_credential(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            endpoint = root / "home/wechat/runtime/xweb_use_endpoint.json"
            endpoint.parent.mkdir(parents=True)
            endpoint.write_text(
                json.dumps({"port": 17321, "token": "private-value", "ws": "/xweb-use/v1"}),
                encoding="utf-8",
            )

            with mock.patch.object(
                finder_runtime_probe, "localhost_reachable", return_value=True
            ):
                result = finder_runtime_probe.probe(
                    root=root, proc_root=root / "missing-proc"
                )

        self.assertTrue(result["ok"])
        self.assertEqual(result["status"], "endpoint_ready")
        self.assertEqual(result["endpointFiles"][0]["port"], 17321)
        self.assertTrue(result["endpointFiles"][0]["hasCredential"])
        self.assertNotIn("private-value", json.dumps(result))

    def test_running_runtime_without_endpoint_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            process = root / "proc/42"
            process.mkdir(parents=True)
            (process / "comm").write_text("WeChatAppEx\n", encoding="utf-8")

            result = finder_runtime_probe.probe(root=root, proc_root=root / "proc")

        self.assertFalse(result["ok"])
        self.assertEqual(result["status"], "endpoint_not_found")
        self.assertEqual(result["runtimeProcessCount"], 1)

    def test_unrelated_json_file_is_not_considered(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            unrelated = root / "tmp/endpoint.json"
            unrelated.parent.mkdir(parents=True)
            unrelated.write_text('{"port":17321}', encoding="utf-8")

            result = finder_runtime_probe.probe(
                root=root, proc_root=root / "missing-proc"
            )

        self.assertFalse(result["ok"])
        self.assertEqual(result["endpointFiles"], [])


if __name__ == "__main__":
    unittest.main()
