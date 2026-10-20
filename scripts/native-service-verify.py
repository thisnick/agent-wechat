#!/usr/bin/env python3
"""Verify the user-unit template against a local build without installing it."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: native-service-verify.py /absolute/path/to/wx")
    binary = Path(sys.argv[1]).resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise SystemExit("native executable must be an executable file")
    # ExecStart specifiers/escapes must not reinterpret the validation path.
    if any(character in str(binary) for character in ('"', "\\", "%", "\n", "\r")):
        raise SystemExit("native executable path contains unsupported unit characters")
    root = Path(__file__).resolve().parent.parent
    source = (root / "deploy/native/agent-wechat-native.service").read_text()
    command = "ExecStart=%h/.local/bin/wx "
    if source.count(command) != 1:
        raise SystemExit("service executable template changed; review validation binding")
    # All remaining unit settings are verified unchanged, including the config
    # path. No binary enters the user's home and no unit is installed or started.
    with tempfile.TemporaryDirectory(prefix="native-unit-verify-") as directory:
        unit = Path(directory) / "agent-wechat-native.service"
        unit.write_text(source.replace(command, f'ExecStart="{binary}" ', 1))
        subprocess.run(["systemd-analyze", "--user", "verify", str(unit)], check=True)


if __name__ == "__main__":
    main()
