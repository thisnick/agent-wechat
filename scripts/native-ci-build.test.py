#!/usr/bin/env python3
"""Check that overlapping setup cannot hide compiler errors or diagnostics."""
import io
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest

helpers = runpy.run_path(str(Path(__file__).with_name("native-ci-build.py")))


class ConcurrentBuildTest(unittest.TestCase):
    def exercise(self, source, expected):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / "compiler.py"
            fixture.write_text(source, encoding="utf-8")
            state = root / "build"
            child = helpers["start_build"](state, [sys.executable, str(fixture)], root)
            try:
                output = io.BytesIO()
                self.assertEqual(helpers["wait_build"](state, timeout=20, output=output), expected)
                child.wait(timeout=5)
                return output.getvalue()
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()

    def test_compiler_success_and_output_are_preserved(self):
        self.assertIn(b"compiled", self.exercise('print("compiled")', 0))

    def test_compiler_failure_and_diagnostics_are_preserved(self):
        self.assertIn(b"compile failed", self.exercise('import sys; print("compile failed", file=sys.stderr); sys.exit(7)', 7))

    def test_dependency_setup_can_run_before_compiler_finishes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / "compiler.py"
            fixture.write_text('from pathlib import Path\nimport time\nwhile not Path("ready").exists(): time.sleep(0.05)\nprint("done")\n')
            state = root / "build"
            child = helpers["start_build"](state, [sys.executable, str(fixture)], root)
            try:
                self.assertIsNone(child.poll())
                self.assertFalse((state / "exit-code").exists())
                (root / "ready").touch()
                self.assertEqual(helpers["wait_build"](state, timeout=20, output=io.BytesIO()), 0)
                child.wait(timeout=5)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()

    def test_compiler_survives_the_launcher_exiting(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state = root / "build"
            fixture = root / "compiler.py"
            fixture.write_text('import time; time.sleep(0.2); print("detached compiler completed")\n')
            launcher = (
                'import runpy, sys; from pathlib import Path; '
                'helpers=runpy.run_path(sys.argv[1]); '
                'helpers["start_build"](Path(sys.argv[2]), [sys.executable, sys.argv[3]], Path(sys.argv[4]))'
            )
            subprocess.run([sys.executable, "-c", launcher,
                            str(Path(__file__).with_name("native-ci-build.py")),
                            str(state), str(fixture), str(root)], check=True, timeout=10)
            output = io.BytesIO()
            self.assertEqual(helpers["wait_build"](state, timeout=20, output=output), 0)
            self.assertIn(b"detached compiler completed", output.getvalue())

    def test_missing_compiler_fails_without_hanging(self):
        with tempfile.TemporaryDirectory() as temporary:
            state = Path(temporary) / "build"
            child = helpers["start_build"](state, [str(Path(temporary) / "missing-compiler")], Path(temporary))
            try:
                self.assertEqual(helpers["wait_build"](state, timeout=20, output=io.BytesIO()), 1)
                child.wait(timeout=5)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()


if __name__ == "__main__":
    unittest.main()
