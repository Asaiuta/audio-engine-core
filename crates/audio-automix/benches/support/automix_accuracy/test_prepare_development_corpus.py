import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import wave

import prepare_development_corpus as development


class Response(io.BytesIO):
    def geturl(self):
        return "https://raw.githubusercontent.com/pinned/audio.wav"


class DevelopmentCorpusTests(unittest.TestCase):
    def test_verified_git_blob_is_reused_and_failed_download_is_not_published(self):
        # Standard Git blob identity, independently reproducible with git hash-object.
        self.assertEqual(development.git_blob_digest(b"hello\n"),
                         "ce013625030ba8dba906f756967f9e9ca394464a")
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "audio.wav"
            digest = development.git_blob_digest(b"original audio")
            with patch.object(development, "urlopen", return_value=Response(b"wrong")):
                with self.assertRaisesRegex(ValueError, "Git blob mismatch"):
                    development.fetch_audio("https://example.org/audio.wav", destination, digest)
            self.assertFalse(destination.exists())
            self.assertEqual(list(Path(directory).iterdir()), [])
            with patch.object(development, "urlopen", return_value=Response(b"original audio")):
                development.fetch_audio("https://example.org/audio.wav", destination, digest)
            with patch.object(development, "urlopen") as network:
                development.fetch_audio("https://example.org/audio.wav", destination, digest)
                network.assert_not_called()
            destination.write_bytes(b"locally changed")
            with self.assertRaisesRegex(ValueError, "cached audio Git blob mismatch"):
                development.fetch_audio("https://example.org/audio.wav", destination, digest)
            self.assertEqual(destination.read_bytes(), b"locally changed")

    def test_download_size_limit_preserves_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "audio.wav"
            with patch.object(development, "MAX_AUDIO_BYTES", 8), \
                    patch.object(development, "urlopen", return_value=Response(b"x" * 9)):
                with self.assertRaisesRegex(ValueError, "size limit"):
                    development.fetch_audio("https://example.org/audio.wav", destination, "a" * 40)
            self.assertFalse(destination.exists())

    def test_native_tempo_and_beat_times_are_preserved_without_range_filtering(self):
        annotation = development.normalize_annotation("233.84\n", "0.217 3\n5.007 1\n5.518 2\n")
        self.assertEqual(annotation["tempo_bpm"], 233.84)
        self.assertEqual(annotation["beats_sec"], [0.217, 5.007, 5.518])
        for invalid in ["0", "nan", "inf", "120 121"]:
            with self.assertRaises(ValueError):
                development.normalize_annotation(invalid, "5 1\n5.5 2\n")
        for invalid in ["5 1\n5 2\n", "5 0\n5.5 2\n", "1 1\n2 2\n"]:
            with self.assertRaises(ValueError):
                development.normalize_annotation("120", invalid)

    def test_native_pcm_coverage_rejects_truncated_audio(self):
        buffer = io.BytesIO()
        with wave.open(buffer, "wb") as audio:
            audio.setparams((1, 2, 8000, 52000, "NONE", "not compressed"))
            audio.writeframes(b"\0" * 104000)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "audio.wav"
            path.write_bytes(buffer.getvalue())
            annotation = {"beats_sec": [5.0, 5.5, 6.0]}
            details = development.validate_audio(path, annotation)
            self.assertEqual(details["frames"], 52000)
            self.assertEqual(details["duration_sec"], 6.5)
            path.write_bytes(buffer.getvalue()[:-100])
            with self.assertRaisesRegex(ValueError, "native PCM coverage"):
                development.validate_audio(path, annotation)

    def test_duplicate_selection_is_order_independent_and_evaluation_overlap_fails(self):
        tracks = [{"id": name, "recording_id": "gtzan:" + name,
                   "audio": {"sha256": name * 64}} for name in ["b", "a"]]
        details = {name: {"pcm_sha256": "same-pcm"} for name in ["a", "b"]}
        evaluation = {"corpora": [{"tracks": []}]}
        included, exclusions = development.select_unique(tracks, details, evaluation)
        self.assertEqual([track["id"] for track in included], ["a"])
        self.assertEqual(exclusions, [{"recording_id": "gtzan:b",
                                      "reason": "exact native PCM duplicate of a"}])
        self.assertEqual((included, exclusions),
                         development.select_unique(list(reversed(tracks)), details, evaluation))
        for identity, digest in [("unrelated-id", "a" * 64), ("gtzan:a", "z" * 64)]:
            evaluation["corpora"][0]["tracks"] = [{"recording_id": identity,
                                                  "audio": {"sha256": digest}}]
            with self.assertRaisesRegex(ValueError, "evaluation overlap"):
                development.select_unique(tracks, details, evaluation)

    def test_changed_evaluation_lock_fails_before_network_or_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            evaluation = root / "evaluation" / "manifest.json"
            evaluation.parent.mkdir()
            evaluation.write_text(json.dumps({"corpora": []}), encoding="utf-8")
            with patch.object(development, "fetch") as network:
                with self.assertRaisesRegex(ValueError, "evaluation manifest SHA-256 mismatch"):
                    development.prepare(root / "development", root / "sources", evaluation, 1)
                network.assert_not_called()
            self.assertFalse((root / "development").exists())


if __name__ == "__main__":
    unittest.main()
