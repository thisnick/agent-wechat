"""Regression tests for raw and build-masked database credential scanning."""

import hashlib
import importlib.util
import io
from pathlib import Path
import struct
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    'extract_keys', Path(__file__).resolve().parents[1] / 'docker/tools/extract-keys.py'
)
extract = importlib.util.module_from_spec(spec)
spec.loader.exec_module(extract)


class MemoryScanTests(unittest.TestCase):
    def scan_fixture(self, mask=None):
        key = hashlib.sha256(b'synthetic database credential fixture').digest()
        stored = key if mask is None else bytes(a ^ b for a, b in zip(key, mask))
        memory = bytearray(0x12000)
        memory[0x11000:0x11010] = extract.CIPHER_CTX_PATTERN
        # Exercise pointer walking rather than just the direct context scan.
        struct.pack_into('<Q', memory, 0x11020, 0x11800)
        struct.pack_into('<Q', memory, 0x11818, 0x11c00)
        memory[0x11c00:0x11c20] = stored

        def open_fixture(path, *args, **kwargs):
            if path == '/proc/4242/maps':
                return io.StringIO('00010000-00012000 rw-p 00000000 00:00 0\n')
            if path == '/proc/4242/mem':
                return io.BytesIO(memory)
            raise AssertionError(f'Unexpected file access: {path}')

        with patch('builtins.open', side_effect=open_fixture):
            count, candidates = extract.extract_candidates(4242, mask)
        self.assertEqual(count, 1)
        self.assertIn(key.hex(), candidates)
        if mask is not None:
            self.assertNotIn(stored.hex(), candidates)

    def test_existing_build_keeps_raw_candidates(self):
        self.scan_fixture()

    def test_masked_build_resolves_pointer_chain_candidate(self):
        self.scan_fixture(extract.BUILD_PROFILES['ce28c347']['db_xor_mask'])

    def test_arm_masked_build_resolves_pointer_chain_candidate(self):
        self.scan_fixture(extract.BUILD_PROFILES['e9f1cd04']['db_xor_mask'])

    def test_bad_mask_fails_before_process_access(self):
        with self.assertRaises(ValueError):
            extract.extract_candidates(4242, b'short')

    def test_unknown_build_preserves_legacy_unmasked_fallback(self):
        with patch.object(extract, 'get_build_id', return_value='unknown'), \
             patch('builtins.print'):
            profile = extract.get_build_profile(4242)
        self.assertIs(profile, extract.BUILD_PROFILES['5233a112'])
        self.assertNotIn('db_xor_mask', profile)


@unittest.skipUnless(shutil.which('sqlcipher'), 'SQLCipher CLI required')
class SqlcipherPageTests(unittest.TestCase):
    def test_page_check_against_real_sqlcipher_database(self):
        key = hashlib.sha256(b'synthetic SQLCipher integration fixture').hexdigest()
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / 'fixture.db'
            subprocess.run(
                ['sqlcipher', str(database)],
                input=f"PRAGMA key = \"x'{key}'\";\nPRAGMA cipher_compatibility=4;\nCREATE TABLE fixture(value TEXT);\nINSERT INTO fixture VALUES ('test');\n",
                text=True, capture_output=True, check=True,
            )
            page = database.read_bytes()[:4096]
            self.assertTrue(extract.matches_sqlcipher4_page(key, page))
            self.assertFalse(extract.matches_sqlcipher4_page('00' * 32, page))
            modified = bytearray(page)
            modified[80] ^= 1
            self.assertFalse(extract.matches_sqlcipher4_page(key, bytes(modified)))
            self.assertFalse(extract.matches_sqlcipher4_page(key, page[:100]))
            self.assertEqual(extract.test_key(str(database), key), '1')


if __name__ == '__main__':
    unittest.main()
