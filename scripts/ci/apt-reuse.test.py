#!/usr/bin/env python3
"""Reuse a cached Ubuntu package only when its SHA-256 matches the package index."""
import contextlib
import hashlib
import io
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest
from unittest import mock

apt_reuse = runpy.run_path(str(Path(__file__).with_name("apt-reuse.py")))

# Captured from apt 2.8.3 on Ubuntu 24.04: an ordinary package, one whose version has an epoch,
# and an architecture-independent one.
PRINT_URIS = """\
'http://archive.ubuntu.com/ubuntu/pool/universe/t/tree/tree_2.1.1-2ubuntu3.24.04.2_amd64.deb' tree_2.1.1-2ubuntu3.24.04.2_amd64.deb 47360 MD5Sum:cf64672d8277fa55b79dd36e6c3947ec
'http://archive.ubuntu.com/ubuntu/pool/main/libx/libxxf86dga/libxxf86dga1_1.1.5-1build1_amd64.deb' libxxf86dga1_2%3a1.1.5-1build1_amd64.deb 11626 MD5Sum:019f211257ee8c34e4ca7208bf764e61
'http://archive.ubuntu.com/ubuntu/pool/main/b/bzip2/bzip2-doc_1.0.8-5.1ubuntu0.1_all.deb' bzip2-doc_1.0.8-5.1ubuntu0.1_all.deb 498722 MD5Sum:fcf6f984533299464f8491ceefbefc74
"""
TREE, DGA, DOC = (line.split()[1] for line in PRINT_URIS.splitlines())

# `apt-cache show` for those three, trimmed to the fields that matter, plus a continuation line.
RECORDS = """\
Package: tree
Architecture: amd64
Version: 2.1.1-2ubuntu3.24.04.2
Filename: pool/universe/t/tree/tree_2.1.1-2ubuntu3.24.04.2_amd64.deb
Size: 47360
SHA256: 56edc302c11b1d35975ded0a38d52e98e82280b12a7a1c7a1b03e4772061731e
Description: displays an indented directory tree, in color
 Tree is a recursive directory listing program: it produces a depth indented listing.

Package: libxxf86dga1
Architecture: amd64
Version: 2:1.1.5-1build1
Filename: pool/main/libx/libxxf86dga/libxxf86dga1_1.1.5-1build1_amd64.deb
Size: 11626
SHA256: bc029b3f329fa95ee46f8b8b826d703971ab74c1c688f8bc74d222db257f9f20
Description: X11 Direct Graphics Access extension library

Package: bzip2-doc
Architecture: all
Version: 1.0.8-5.1ubuntu0.1
Filename: pool/main/b/bzip2/bzip2-doc_1.0.8-5.1ubuntu0.1_all.deb
Size: 498722
SHA256: 4929bbebb3b117c799ea18be027eabe77c0902689f7fb3d25db01380a1d0d6bc
Description: high-quality block-sorting file compressor - documentation

"""


def digest(data):
    return hashlib.sha256(data).hexdigest()


class AptReuseTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.saved = Path(temporary.name, "saved")
        self.archives = Path(temporary.name, "archives")
        self.saved.mkdir()
        self.archives.mkdir()

    def staged(self):
        return sorted(path.name for path in self.archives.iterdir())

    def test_reads_real_apt_output(self):
        self.assertEqual(apt_reuse["parse_print_uris"](PRINT_URIS), [TREE, DGA, DOC])
        self.assertEqual(apt_reuse["identity"](DGA), ("libxxf86dga1", "2:1.1.5-1build1", "amd64"))
        self.assertEqual(apt_reuse["identity"](DOC), ("bzip2-doc", "1.0.8-5.1ubuntu0.1", "all"))
        hashes = apt_reuse["parse_records"](RECORDS)
        self.assertEqual(hashes[apt_reuse["identity"](TREE)],
                         {"56edc302c11b1d35975ded0a38d52e98e82280b12a7a1c7a1b03e4772061731e"})
        self.assertEqual(len(hashes), 3)

    def test_keeps_each_architecture_hash_apart(self):
        foreign = RECORDS.replace("Architecture: amd64\nVersion: 2.1.1", "Architecture: i386\nVersion: 2.1.1")
        hashes = apt_reuse["parse_records"](RECORDS + foreign)
        self.assertEqual(len(hashes[("tree", "2.1.1-2ubuntu3.24.04.2", "amd64")]), 1)
        self.assertIn(("tree", "2.1.1-2ubuntu3.24.04.2", "i386"), hashes)

    def test_reuses_only_files_that_match_the_index(self):
        good, tampered = b"good!!", b"evil!!"  # same size: apt alone would accept either
        (self.saved / TREE).write_bytes(good)
        (self.saved / DGA).write_bytes(tampered)
        hashes = {apt_reuse["identity"](TREE): {digest(good)},
                  apt_reuse["identity"](DGA): {digest(b"other!")}}

        reused, download, rejected = apt_reuse["stage"](self.saved, self.archives, [TREE, DGA, DOC], hashes)

        self.assertEqual((reused, download, rejected), ([TREE], [DGA, DOC], [DGA]))
        self.assertEqual(self.staged(), [TREE])
        self.assertEqual((self.archives / TREE).read_bytes(), good)

    def test_downloads_a_file_the_index_does_not_list(self):
        (self.saved / TREE).write_bytes(b"good!!")
        result = apt_reuse["stage"](self.saved, self.archives, [TREE], {})
        self.assertEqual(result, ([], [TREE], [TREE]))
        self.assertEqual(self.staged(), [])

    def test_asks_apt_with_an_empty_archive_and_exact_architectures(self):
        calls = []

        def run(command, **_):
            calls.append(command)
            output = PRINT_URIS if command[0] == "apt-get" else RECORDS
            return subprocess.CompletedProcess(command, 0, stdout=output)

        with mock.patch("subprocess.run", run):
            files = apt_reuse["needed_files"](["tree", "libxxf86dga1", "bzip2-doc"])
            hashes = apt_reuse["index_hashes"](files)
        self.assertEqual(files, [TREE, DGA, DOC])
        self.assertEqual(len(hashes), 3)
        self.assertRegex(calls[0][5], r"^Dir::Cache::Archives=/")
        self.assertEqual(calls[1][2:], ["bzip2-doc:all=1.0.8-5.1ubuntu0.1", "libxxf86dga1:amd64=2:1.1.5-1build1",
                                        "tree:amd64=2.1.1-2ubuntu3.24.04.2"])

    def test_an_error_stages_nothing_and_lets_apt_download(self):
        def broken(_packages):
            raise subprocess.CalledProcessError(100, "apt-get")

        output = io.StringIO()
        with mock.patch.dict(apt_reuse["main"].__globals__, {"needed_files": broken}), \
                contextlib.redirect_stdout(output):
            apt_reuse["main"](["--saved", str(self.saved), "--archives", str(self.archives), "tree"])
        self.assertIn("::warning title=Cached Ubuntu packages not reused::CalledProcessError", output.getvalue())
        self.assertEqual(self.staged(), [])


if __name__ == "__main__":
    unittest.main()
