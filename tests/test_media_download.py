import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'docker/tools' / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


worker = load('media_download', 'media-download.py')
convert = load('media_convert', 'media-convert.py')


class DownloadGuards(unittest.TestCase):
    def fixture(self, **updates):
        value = dict(chatId='test-peer', accountId='test-account', local_id=42,
                     local_type=3, server_id='9007199254740993', sort_seq='123',
                     create_time=1790038000, content='<msg><img /></msg>')
        value.update(updates)
        return value

    def test_completed_file_and_lossless_ids(self):
        result = worker.validate_metadata(self.fixture(local_type=(6 << 32) | 49, offsets={}, dryRun=True))
        self.assertEqual(result['server_id'], '9007199254740993')
        self.assertNotIn('offsets', result)
        self.assertNotIn('dryRun', result)

    def test_rejects_unvalidated_targets_before_attach(self):
        for change in [dict(local_type=(74 << 32) | 49), dict(local_type=34),
                       dict(local_id=True), dict(local_id=0), dict(local_id=2**32),
                       dict(server_id=42), dict(server_id=str(2**64)),
                       dict(chatId='group@chatroom'), dict(chatId='filehelper'),
                       dict(chatId='test-account'), dict(chatId='a\nb'),
                       dict(accountId=''), dict(content='x\0y'),
                       dict(content='x' * (1024 * 1024 + 1))]:
            with self.subTest(change=list(change)), self.assertRaises(ValueError):
                worker.validate_metadata(self.fixture(**change))

    def test_requires_full_known_build_id(self):
        for build in worker.BUILDS:
            with patch.object(subprocess, 'check_output', return_value='Build ID: ' + build):
                self.assertEqual(worker.build_id(123), build)
        for notes in ('Build ID: ce28c347', '', 'Build ID: ' + '0' * 40,
                      '\n'.join('Build ID: ' + b for b in worker.BUILDS)):
            with patch.object(subprocess, 'check_output', return_value=notes), self.assertRaises(ValueError):
                worker.build_id(123)

    def test_incomplete_image_never_reaches_codec(self):
        with patch.object(subprocess, 'run') as run:
            for data in (b'\x89PNG\r\n\x1a\npartial', b'\xff\xd8partial', b'GIF89apartial', b'unknown'):
                with self.assertRaises(ValueError):
                    convert.validate_image(data)
            run.assert_not_called()

    def test_codec_errors_reject_complete_looking_image(self):
        data = b'\xff\xd8invalid\xff\xd9'
        with patch.object(subprocess, 'run', return_value=subprocess.CompletedProcess([], 1, b'', b'error')):
            with self.assertRaises(ValueError):
                convert.validate_image(data)


if __name__ == '__main__':
    unittest.main()
