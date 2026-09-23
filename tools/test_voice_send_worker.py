import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import wave

WORKER_PATH = Path(__file__).resolve().parents[1] / "docker" / "tools" / "voice-send-worker"
loader = importlib.machinery.SourceFileLoader("voice_send_worker", str(WORKER_PATH))
spec = importlib.util.spec_from_loader(loader.name, loader)
worker = importlib.util.module_from_spec(spec)
loader.exec_module(worker)


class VoicePreparationTests(unittest.TestCase):
    def test_boundaries(self):
        cases = {
            49.9: [49.9],
            50: [50],
            50.1: [48.1, 2],
            100: [50, 50],
            100.1: [50, 48.1, 2],
            120: [50, 50, 20],
        }
        for seconds, expected in cases.items():
            with self.subTest(seconds=seconds):
                total = round(seconds * worker.RATE)
                parts = worker.bounds(total)
                actual = [(end - start) / worker.RATE for start, end in parts]
                self.assertEqual(actual, expected)
                self.assertEqual(parts[0][0], 0)
                self.assertEqual(parts[-1][1], total)
                self.assertTrue(all(parts[i][1] == parts[i + 1][0] for i in range(len(parts) - 1)))
                self.assertTrue(all(end - start <= worker.MAX_SAMPLES for start, end in parts))

    def test_prepare_reconstructs_source_and_adds_padding(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            samples = (b"\x01\x00\xff\x7f" * (round(50.1 * worker.RATE) // 2))
            with wave.open(str(directory / "input"), "wb") as audio:
                audio.setnchannels(1)
                audio.setsampwidth(2)
                audio.setframerate(worker.RATE)
                audio.writeframes(samples)
            worker.save(directory / "status.json", {"jobId": "test", "chatId": "filehelper", "status": "queued", "chunks": []})
            worker.prepare(directory)
            status = json.loads((directory / "status.json").read_text())
            self.assertEqual(status["status"], "prepared")
            self.assertEqual(len(status["chunks"]), 2)
            self.assertEqual([part["sourceDurationSeconds"] for part in status["chunks"]], [48.1, 2])
            self.assertEqual([part["playbackDurationSeconds"] for part in status["chunks"]], [48.6, 2.5])
            self.assertEqual(worker.read_pcm(directory / "source.wav"), samples)

    def test_short_input_is_padded_to_two_seconds(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            samples = b"\x01\x00" * worker.RATE
            with wave.open(str(directory / "input"), "wb") as audio:
                audio.setnchannels(1)
                audio.setsampwidth(2)
                audio.setframerate(worker.RATE)
                audio.writeframes(samples)
            worker.save(directory / "status.json", {"jobId": "test", "chatId": "filehelper", "status": "queued", "chunks": []})
            worker.prepare(directory)
            status = json.loads((directory / "status.json").read_text())
            self.assertEqual(status["chunks"][0]["sourceDurationSeconds"], 1)
            self.assertEqual(status["chunks"][0]["playbackDurationSeconds"], 2.5)
            prepared = worker.read_pcm(directory / "chunk-001.wav")
            self.assertEqual(prepared[worker.EDGE_SAMPLES * 2:(worker.EDGE_SAMPLES + worker.RATE) * 2], samples)


if __name__ == "__main__":
    unittest.main()
