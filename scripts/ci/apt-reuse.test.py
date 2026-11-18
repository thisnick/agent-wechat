#!/usr/bin/env python3
"""Reuse a cached Ubuntu package only when its SHA-256 matches the package index."""
import hashlib
from pathlib import Path
import runpy
import tempfile
import unittest

apt_reuse = runpy.run_path(str(Path(__file__).with_name("apt-reuse.py")))

PRINT_URIS = """\
'http://archive.ubuntu.com/ubuntu/pool/universe/t/tree/tree_2.1.1-2_amd64.deb' tree_2.1.1-2_amd64.deb 6 MD5Sum:0
'http://archive.ubuntu.com/ubuntu/pool/main/libx/libxxf86dga/libxxf86dga1_1.1.5-1_amd64.deb' libxxf86dga1_2%3a1.1.5-1_amd64.deb 7 MD5Sum:0
'http://archive.ubuntu.com/ubuntu/pool/main/f/fonts/fonts-x_1.0_all.deb' fonts-x_1.0_all.deb 5 MD5Sum:0
"""


def digest(data):
    return hashlib.sha256(data).hexdigest()


def records(*entries):
    return "\n\n".join(
        f"Package: {name}\nArchitecture: {arch}\nVersion: {version}\n"
        f"Description: a package\n with a continuation line: that has a colon\nSHA256: {sha}\n"
        for name, version, arch, sha in entries) + "\n"


class AptReuseTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.saved = Path(temporary.name, "saved")
        self.archives = Path(temporary.name, "archives")
        self.saved.mkdir()
        self.archives.mkdir()

    def test_reads_apt_file_names_and_unescapes_epochs(self):
        files = apt_reuse["parse_print_uris"](PRINT_URIS)
        self.assertEqual(files, ["tree_2.1.1-2_amd64.deb", "libxxf86dga1_2%3a1.1.5-1_amd64.deb",
                                 "fonts-x_1.0_all.deb"])
        self.assertEqual(apt_reuse["identity"](files[1]), ("libxxf86dga1", "2:1.1.5-1", "amd64"))
        self.assertEqual(apt_reuse["identity"](files[2]), ("fonts-x", "1.0", "all"))

    def test_keeps_each_architecture_hash_apart(self):
        hashes = apt_reuse["parse_records"](records(
            ("tree", "2.1.1-2", "amd64", "a" * 64), ("tree", "2.1.1-2", "i386", "b" * 64)))
        self.assertEqual(hashes[("tree", "2.1.1-2", "amd64")], {"a" * 64})
        self.assertEqual(hashes[("tree", "2.1.1-2", "i386")], {"b" * 64})

    def test_reuses_only_files_that_match_the_index(self):
        good, tampered = b"good!!", b"evil!!"  # same size: apt alone would accept either
        (self.saved / "tree_2.1.1-2_amd64.deb").write_bytes(good)
        (self.saved / "libxxf86dga1_2%3a1.1.5-1_amd64.deb").write_bytes(tampered)
        hashes = apt_reuse["parse_records"](records(
            ("tree", "2.1.1-2", "amd64", digest(good)),
            ("libxxf86dga1", "2:1.1.5-1", "amd64", digest(b"other!")),
            ("fonts-x", "1.0", "all", digest(b"fonts"))))
        files = apt_reuse["parse_print_uris"](PRINT_URIS)

        reused, download, rejected = apt_reuse["stage"](self.saved, self.archives, files, hashes)

        self.assertEqual(reused, ["tree_2.1.1-2_amd64.deb"])
        self.assertEqual(download, ["libxxf86dga1_2%3a1.1.5-1_amd64.deb", "fonts-x_1.0_all.deb"])
        self.assertEqual(rejected, ["libxxf86dga1_2%3a1.1.5-1_amd64.deb"])
        self.assertEqual(sorted(path.name for path in self.archives.iterdir()), ["tree_2.1.1-2_amd64.deb"])
        self.assertEqual((self.archives / "tree_2.1.1-2_amd64.deb").read_bytes(), good)

    def test_downloads_a_file_the_index_does_not_list(self):
        (self.saved / "tree_2.1.1-2_amd64.deb").write_bytes(b"good!!")
        reused, download, rejected = apt_reuse["stage"](
            self.saved, self.archives, ["tree_2.1.1-2_amd64.deb"], {})
        self.assertEqual((reused, download, rejected),
                         ([], ["tree_2.1.1-2_amd64.deb"], ["tree_2.1.1-2_amd64.deb"]))


if __name__ == "__main__":
    unittest.main()
