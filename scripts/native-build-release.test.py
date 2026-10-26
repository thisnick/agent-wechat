#!/usr/bin/env python3
"""Check release-builder reuse and source admission with synthetic Docker output.

The real executable/image smoke tests still run separately on each CI worker.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
COMMIT = "a" * 40


class ReleaseBuilderTests(unittest.TestCase):
    def exercise(self, *, cached=True, arch="x86_64", revision=COMMIT, platform="linux/amd64", handoff=False, compiler="rustc 1.93.1 (fixture)"):
        with tempfile.TemporaryDirectory(prefix="native-release-builder-") as temporary:
            directory = Path(temporary)
            binaries = directory / "bin"
            binaries.mkdir()
            log = directory / "docker.jsonl"
            docker = f"#!{sys.executable}\n" + '''import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with open(os.environ["FIXTURE_LOG"], "a") as stream:
    stream.write(json.dumps(args) + "\\n")
if args[:2] == ["image", "inspect"]:
    key = "FIXTURE_REVISION" if "revision" in args[3] else "FIXTURE_PLATFORM"
    print(os.environ[key])
elif args[0] == "create":
    print("synthetic-container")
elif args[0] == "cp":
    if args[1].endswith("/build/rustc-version"):
        Path(args[-1]).write_text(os.environ["FIXTURE_COMPILER"] + "\\n")
    else:
        Path(args[-1]).write_text('#!/bin/sh\\nprintf "%s\\\\n" "$FIXTURE_IDENTITY"\\n')
'''
            stubs = {
                "docker": docker,
                "git": '#!/bin/sh\nif [ "$1" = rev-parse ]; then printf "%s\\n" "$FIXTURE_COMMIT"; fi\n',
                "uname": '#!/bin/sh\nif [ "$1" = -s ]; then echo Linux; else printf "%s\\n" "$FIXTURE_ARCH"; fi\n',
                "python3": "#!/bin/sh\nexit 0\n",
            }
            for name, content in stubs.items():
                path = binaries / name
                path.write_text(content)
                path.chmod(0o755)
            environment = dict(os.environ)
            identity = subprocess.check_output(
                ["node", "--input-type=module", "-e",
                 "import {targetFor} from './packages/cli/scripts/native-runtime.mjs';"
                 "import fs from 'node:fs';"
                 "console.log(JSON.stringify({product:'agent-wechat',backend:'native',"
                 "version:JSON.parse(fs.readFileSync('packages/cli/package.json')).version,"
                 "target:targetFor().rustTarget,apiVersion:1,capabilities:['serve']}));"],
                cwd=ROOT, text=True,
            ).strip()
            environment.update(
                PATH=str(binaries) + os.pathsep + environment["PATH"],
                FIXTURE_LOG=str(log),
                FIXTURE_COMMIT=COMMIT,
                FIXTURE_ARCH=arch,
                FIXTURE_REVISION=revision,
                FIXTURE_PLATFORM=platform,
                FIXTURE_IDENTITY=identity,
                FIXTURE_COMPILER=compiler,
                NATIVE_CI_ARTIFACT="true" if handoff else "false",
            )
            environment.pop("NATIVE_RELEASE_BUILDER_IMAGE", None)
            if cached:
                environment["NATIVE_RELEASE_BUILDER_IMAGE"] = "synthetic-builder:ci"
            output = directory / "artifacts"
            result = subprocess.run(
                ["bash", str(ROOT / "scripts/native-build-release.sh"), str(output)],
                env=environment, capture_output=True, text=True, timeout=30,
            )
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            assets = [path.name for path in output.iterdir()]
            return result, calls, assets

    def test_cached_builder_extracts_without_rebuilding(self):
        for arch, platform, asset_arch in [
            ("x86_64", "linux/amd64", "x64"),
            ("aarch64", "linux/arm64", "arm64"),
        ]:
            with self.subTest(arch=arch):
                result, calls, assets = self.exercise(arch=arch, platform=platform)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse(any(call[0] == "build" for call in calls))
                self.assertIn(["create", "synthetic-builder:ci"], calls)
                self.assertIn(["rm", "synthetic-container"], calls)
                self.assertTrue(any(name.endswith(f"-linux-{asset_arch}-gnu") for name in assets))
                self.assertTrue(any(name.endswith("-checksums.json") for name in assets))

    def test_cached_builder_rejects_another_commit(self):
        result, calls, assets = self.exercise(revision="b" * 40)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must match", result.stderr)
        self.assertFalse(any(call[0] in ["build", "create", "cp"] for call in calls))
        self.assertEqual(assets, [])

    def test_cached_builder_rejects_another_architecture(self):
        result, calls, assets = self.exercise(platform="linux/arm64")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must match", result.stderr)
        self.assertFalse(any(call[0] in ["build", "create", "cp"] for call in calls))
        self.assertEqual(assets, [])

    def test_local_build_still_builds_before_extracting(self):
        result, calls, assets = self.exercise(cached=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls[0][:3], ["build", "--platform", "linux/amd64"])
        self.assertIn("builder", calls[0])
        self.assertFalse(any(call[:2] == ["image", "inspect"] for call in calls))
        self.assertTrue(any(name.endswith("-checksums.json") for name in assets))

    def test_ci_handoff_records_the_compiler_from_the_actual_builder(self):
        result, calls, assets = self.exercise(handoff=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("native-ci-build.json", assets)
        self.assertTrue(any(call[0] == "cp" and call[1].endswith("/build/rustc-version") for call in calls))

    def test_ci_handoff_rejects_an_unexpected_builder_compiler(self):
        result, calls, assets = self.exercise(handoff=True, compiler="rustc 1.94.0 (wrong compiler)")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("pinned Rust toolchain", result.stderr)
        self.assertNotIn("native-ci-build.json", assets)


if __name__ == "__main__":
    unittest.main()
