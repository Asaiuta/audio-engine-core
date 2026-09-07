import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import prepare_public_corpus as public
import provision_corpus as provisioning


class Response(io.BytesIO):
    def geturl(self):
        return "https://corpus.example/source"


class PublicCorpusTests(unittest.TestCase):
    def test_tempo_uses_salience_and_lower_bpm_on_ties(self):
        self.assertEqual(public.normalize_tempo("127 139 0.9487179487179488")["tempo_bpm"], 127)
        self.assertEqual(public.normalize_tempo("65 129 0.1")["tempo_bpm"], 129)
        self.assertEqual(public.normalize_tempo("140 70 0.5")["tempo_bpm"], 70)
        self.assertEqual(public.normalize_tempo("128 0 1")["tempo_bpm"], 128)
        for label in ["0 0 0", "nan 120 1", "120 240 1.1", "120 0 0.5", "120 1"]:
            with self.subTest(label=label), self.assertRaises(ValueError):
                public.normalize_tempo(label)

    def test_beats_keep_native_seconds_and_all_bar_positions(self):
        annotation = public.normalize_beats("4.5 4\n5.01 1\n5.512345678 2\n6.01 3\n")
        self.assertEqual(annotation, {"schema_version": 1,
                                      "beats_sec": [4.5, 5.01, 5.512345678, 6.01]})
        for label in ["5 1\n5 2", "6 1\n5 2", "nan 1", "5 -1", "5 1 2", "0 1\n1 2"]:
            with self.subTest(label=label), self.assertRaises(ValueError):
                public.normalize_beats(label)

    def test_replica_selection_is_independent_of_order_and_results(self):
        readme = "\n".join(f"    Waltz/B{i}.wav matches Waltz/A{i}.wav" for i in range(13))
        self.assertEqual(public.ballroom_exclusions(readme), {f"B{i}": f"A{i}" for i in range(13)})
        with self.assertRaises(ValueError):
            public.ballroom_exclusions(readme + "\n Waltz/Z.wav matches Waltz/A0.wav")

    def test_archive_inspection_rejects_escapes_links_duplicates_and_size_abuse(self):
        for name in ["../escape", "/absolute", "C:/drive", "a\\b", "a//b", "a/./b"]:
            with self.subTest(name=name), self.assertRaises(ValueError):
                public.inspect_members([(name, 1, True)], 10, 100)
        for members in [[("link", 1, False)], [("a", 1, True), ("A", 1, True)],
                        [("a", 101, True)], [("a", -1, True)]]:
            with self.subTest(members=members), self.assertRaises(ValueError):
                public.inspect_members(members, 10, 100)
        with self.assertRaises(ValueError):
            public.inspect_members([("a", 1, True), ("b", 1, True)], 1, 100)

    def test_pinned_zip_is_read_without_executing_or_extracting_scripts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "source.zip"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("source/labels.beats", "5.0 1\n5.5 2\n")
                output.writestr("source/audio_dl.sh", "exit 1")
            files, inspection = public.read_annotations(archive)
            self.assertEqual(files["labels.beats"], "5.0 1\n5.5 2\n")
            self.assertEqual(inspection["entries"], 2)
            self.assertEqual(list(root.iterdir()), [archive])

    def test_upstream_md5_and_download_limit_preserve_existing_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "audio.mp3"
            destination.write_bytes(b"old")
            with patch.object(provisioning, "urlopen", return_value=Response(b"abc")):
                provisioning.download("https://corpus.example/audio", destination,
                                      "900150983cd24fb0d6963f7d28e17f72", algorithm="md5")
            with patch.object(provisioning, "urlopen", return_value=Response(b"changed")):
                with self.assertRaisesRegex(ValueError, "MD5 mismatch"):
                    provisioning.download("https://corpus.example/audio", destination,
                                          "900150983cd24fb0d6963f7d28e17f72", algorithm="md5")
            with patch.object(provisioning, "urlopen", return_value=Response(b"too long")):
                with self.assertRaisesRegex(ValueError, "size limit"):
                    provisioning.download("https://corpus.example/audio", destination,
                                          "0" * 64, max_bytes=3)
            self.assertEqual(destination.read_bytes(), b"abc")
            with patch.object(provisioning, "urlopen") as network:
                public.fetch("https://corpus.example/audio", destination,
                             "900150983cd24fb0d6963f7d28e17f72", 3, "md5")
                network.assert_not_called()


if __name__ == "__main__":
    unittest.main()
