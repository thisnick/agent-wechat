#!/usr/bin/env python3
"""Dependency cache identities for CI (Python 3.11+, no extra packages)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import tomllib


def cache_digest(root: Path, scope: str) -> str:
    inputs = {"schema": 1, "scope": scope}

    def include(path: Path, value):
        inputs[path.relative_to(root).as_posix()] = value

    if scope == "rust":
        workspace = root / "packages/native-client-rust"
        manifests = [workspace / "Cargo.toml", *sorted(workspace.glob("crates/*/Cargo.toml"))]
        for path in manifests:
            manifest = tomllib.loads(path.read_text(encoding="utf-8"))
            # Package releases do not change compiled third-party dependencies.
            manifest.get("package", {}).pop("version", None)
            manifest.get("workspace", {}).get("package", {}).pop("version", None)
            include(path, manifest)
        lock = tomllib.loads((workspace / "Cargo.lock").read_text(encoding="utf-8"))
        lock["package"] = [package for package in lock["package"]
                           if "source" in package or "checksum" in package]
        include(workspace / "Cargo.lock", lock)
        for directory in [root, workspace, *(path.parent for path in manifests[1:])]:
            for name in [".cargo/config", ".cargo/config.toml"]:
                path = directory / name
                if path.is_file():
                    include(path, path.read_text(encoding="utf-8"))
    elif scope == "pnpm":
        for path in [root / "package.json", *sorted((root / "packages").glob("*/package.json"))]:
            package = json.loads(path.read_text(encoding="utf-8"))
            package.pop("version", None)
            include(path, package)
        for name in ["pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc"]:
            path = root / name
            if path.is_file():
                include(path, path.read_text(encoding="utf-8"))
    else:
        raise ValueError(f"Unknown dependency scope: {scope}")
    return hashlib.sha256(json.dumps(inputs, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scope", choices=["rust", "pnpm", "all"])
    parser.add_argument("--github-output", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    scopes = ["rust", "pnpm"] if args.scope == "all" else [args.scope]
    for scope in scopes:
        digest = cache_digest(root, scope)
        if args.github_output:
            with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
                output.write(f"{scope}={digest}\n")
        else:
            print(f"{scope}={digest}")
