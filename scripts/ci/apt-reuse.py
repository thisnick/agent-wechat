#!/usr/bin/env python3
"""Reuse cached Ubuntu packages only when they match the package index (Python 3.9+, no extra packages).

apt installs a file already in its archives directory when only its size matches the index,
without checking its hash. Before the install, this copies each cached .deb file the install
needs into that directory only if its SHA-256 matches the signed index that apt-get update
just fetched. apt downloads anything missing or different as usual.

Reuse only saves time, so any error here stages nothing and lets apt download everything.
"""
import argparse
import hashlib
from pathlib import Path
import subprocess
import sys
import tempfile
from urllib.parse import unquote


def parse_print_uris(output):
    """File names from `apt-get --print-uris`, whose lines read: 'URI' FILE SIZE HASH.

    apt 2.8 gives only an MD5 sum in HASH, so the SHA-256 comes from the index instead.
    """
    return [line.split()[1] for line in output.splitlines() if line.startswith("'")]


def identity(filename):
    """(package, version, architecture) from apt's name for a download, which escapes ':' as %3a."""
    name, version, architecture = filename.removesuffix(".deb").split("_")
    return unquote(name), unquote(version), unquote(architecture)


def parse_records(output):
    """The SHA-256 values `apt-cache show` lists for each (package, version, architecture)."""
    hashes = {}
    for record in output.split("\n\n"):
        fields = dict(line.split(": ", 1) for line in record.splitlines()
                      if ": " in line and not line.startswith(" "))
        if {"Package", "Version", "Architecture", "SHA256"} <= fields.keys():
            key = (fields["Package"], fields["Version"], fields["Architecture"])
            hashes.setdefault(key, set()).add(fields["SHA256"].strip())
    return hashes


def copy_if_matches(source, destination, expected):
    """Copy `source` to `destination` if its SHA-256 is in `expected`, reading it once."""
    digest = hashlib.sha256()
    partial = destination.with_name(destination.name + ".partial")
    with open(source, "rb") as reader, open(partial, "wb") as writer:
        for block in iter(lambda: reader.read(1 << 20), b""):
            digest.update(block)
            writer.write(block)
    if digest.hexdigest() in expected:
        partial.replace(destination)
        return True
    partial.unlink()
    return False


def stage(saved, archives, files, hashes):
    """Copy each file in `files` from `saved` to `archives` when its SHA-256 is one the index lists.

    Returns the files copied, the files apt must download, and the cached files that did not match.
    """
    reused, download, rejected = [], [], []
    for filename in files:
        cached = saved / filename
        if not cached.is_file():
            download.append(filename)
        elif copy_if_matches(cached, archives / filename, hashes.get(identity(filename), set())):
            reused.append(filename)
        else:
            download.append(filename)
            rejected.append(filename)
    return reused, download, rejected


def needed_files(packages):
    """The .deb files apt would download to install `packages`, starting from an empty archive."""
    with tempfile.TemporaryDirectory() as empty:
        output = subprocess.run(
            ["apt-get", "install", "-qq", "--print-uris", "-o", f"Dir::Cache::Archives={empty}", *packages],
            check=True, stdout=subprocess.PIPE, text=True).stdout
    return parse_print_uris(output)


def index_hashes(files):
    specs = sorted({f"{name}:{architecture}={version}" for name, version, architecture in map(identity, files)})
    # A spec the index lacks makes apt-cache exit non-zero; its file is then simply downloaded.
    output = subprocess.run(["apt-cache", "show", *specs], stdout=subprocess.PIPE, text=True).stdout
    return parse_records(output)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--saved", type=Path, required=True,
                        help="directory the Actions cache restored .deb files into")
    parser.add_argument("--archives", type=Path, required=True,
                        help="the empty apt archives directory (Dir::Cache::Archives) the install will use")
    parser.add_argument("packages", nargs="+", help="Ubuntu packages to install")
    args = parser.parse_args(argv)

    try:
        files = needed_files(args.packages)
        hashes = index_hashes(files) if files else {}
        reused, download, rejected = stage(args.saved, args.archives, files, hashes)
    except Exception as error:  # noqa: BLE001 - reuse is optional; apt downloads instead
        print(f"::warning title=Cached Ubuntu packages not reused::{type(error).__name__}: {error}")
        return
    print(f"{len(files)} package files needed: {len(reused)} reused from the cache, "
          f"{len(download)} to download.")
    for filename in rejected:
        print(f"Not reusing {filename}: its SHA-256 differs from the package index.")


if __name__ == "__main__":
    sys.exit(main())
