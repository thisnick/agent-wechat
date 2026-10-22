#!/usr/bin/env python3
"""Overlap the native release compiler with independent CI dependency setup."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def start_build(state: Path, command: list[str], cwd: Path):
    state.mkdir(parents=True)
    (state / "request.json").write_text(json.dumps({"command": command, "cwd": str(cwd)}), encoding="utf-8")
    options = {"start_new_session": True} if os.name != "nt" else {
        "creationflags": subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP,
    }
    # Preserve RUNNER_TRACKING_ID so Actions also cleans up this process on cancel.
    child = subprocess.Popen(
        [sys.executable, str(Path(__file__).resolve()), "_compile", "--state", str(state)],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        close_fds=True, **options,
    )
    (state / "pid").write_text(str(child.pid), encoding="ascii")
    return child


def process_alive(pid: int) -> bool:
    if os.name != "nt":
        try:
            os.kill(pid, 0)
            return True
        except ProcessLookupError:
            return False
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.OpenProcess(0x100000, False, pid)  # SYNCHRONIZE, query only.
    if not handle:
        return False
    try:
        return kernel.WaitForSingleObject(handle, 0) == 258  # WAIT_TIMEOUT
    finally:
        kernel.CloseHandle(handle)


def compile_build(state: Path):
    result = 1
    with (state / "compiler.log").open("wb") as output:
        try:
            request = json.loads((state / "request.json").read_text(encoding="utf-8"))
            result = subprocess.run(request["command"], cwd=request["cwd"],
                                    stdin=subprocess.DEVNULL, stdout=output,
                                    stderr=subprocess.STDOUT, check=False).returncode
        except Exception as error:
            output.write(f"Cannot launch native compiler: {error}\n".encode())
    temporary = state / "exit-code.tmp"
    temporary.write_text(str(result), encoding="ascii")
    temporary.replace(state / "exit-code")


def wait_build(state: Path, timeout=3300, output=None):
    if not (state / "request.json").is_file():
        raise RuntimeError("Native release compiler was not started")
    output = sys.stdout.buffer if output is None else output
    deadline = time.monotonic() + timeout
    offset = 0
    while True:
        complete = (state / "exit-code").is_file()
        log = state / "compiler.log"
        if log.is_file():
            with log.open("rb") as stream:
                stream.seek(offset)
                chunk = stream.read()
                offset += len(chunk)
                output.write(chunk)
                output.flush()
        if complete:
            return int((state / "exit-code").read_text(encoding="ascii"))
        if not process_alive(int((state / "pid").read_text(encoding="ascii"))):
            raise RuntimeError("Native compiler exited without recording its result")
        if time.monotonic() >= deadline:
            raise TimeoutError("Native compiler did not finish before the CI deadline")
        time.sleep(0.1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["start", "wait", "_compile"])
    parser.add_argument("target", nargs="?")
    parser.add_argument("--state", type=Path)
    args = parser.parse_args()
    state = args.state or Path(os.environ["RUNNER_TEMP"]) / "native-release-build"
    if args.operation == "start":
        if not args.target:
            parser.error("start requires a Rust target")
        root = Path(__file__).resolve().parent.parent
        start_build(state, ["cargo", "build", "--release", "--locked", "--manifest-path",
                           "packages/native-client-rust/Cargo.toml", "--target", args.target], root)
        print("Native release compiler started; dependency setup can run concurrently.")
    elif args.operation == "_compile":
        compile_build(state)
    else:
        sys.exit(wait_build(state))
