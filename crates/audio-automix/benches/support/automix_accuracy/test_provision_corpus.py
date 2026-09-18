import copy
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import provision_corpus as provisioning


class Response(io.BytesIO):
    def geturl(self):
        return "https://corpus.example/data"


class ProvisioningTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.content = b"locally generated test bytes"
        digest = hashlib.sha256(self.content).hexdigest()
        self.manifest = {
            "schema_version": 1,
            "corpora": [
                {
                    "id": corpus_id,
                    "metrics": sorted(metrics),
                    "tracks": [{
                        "audio": {"path": f"{corpus_id}/audio.wav", "sha256": digest},
                        "annotation": {"path": f"{corpus_id}/labels.json", "sha256": digest},
                    }],
                }
                for corpus_id, metrics in provisioning.REQUIRED_METRICS.items()
            ],
        }

    def run_provisioning(self, manifest, content=None, digest=None):
        encoded = json.dumps(manifest).encode()
        with patch.object(provisioning, "urlopen") as fetch:
            fetch.side_effect = lambda url, **kwargs: Response(
                encoded if url.endswith("manifest.json") else
                self.content if content is None else content
            )
            result = provisioning.provision(
                "https://corpus.example/manifest.json",
                digest or hashlib.sha256(encoded).hexdigest(),
                "https://corpus.example/data", self.root,
            )
            return result, fetch.call_count

    def test_exact_bytes_are_verified_and_materialized(self):
        path, count = self.run_provisioning(self.manifest)
        self.assertEqual(count, 5)
        self.assertEqual(json.loads(path.read_text()), self.manifest)
        self.assertEqual((self.root / "ballroom-beats/audio.wav").read_bytes(), self.content)

    def test_manifest_and_artifact_hash_mismatches_fail(self):
        with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
            self.run_provisioning(self.manifest, digest="0" * 64)
        with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
            self.run_provisioning(self.manifest, content=b"corrupted")
        self.assertFalse((self.root / "ballroom-beats/audio.wav").exists())

    def test_required_corpora_and_metric_sets_cannot_be_omitted(self):
        for index, metrics in [(0, ["tempo_accuracy1"]), (1, ["beat_f_measure"])]:
            manifest = copy.deepcopy(self.manifest)
            manifest["corpora"][index]["metrics"] = metrics
            with self.assertRaisesRegex(ValueError, "required evaluation metrics"):
                self.run_provisioning(manifest)
        missing = copy.deepcopy(self.manifest)
        missing["corpora"].pop()
        with self.assertRaisesRegex(ValueError, "required evaluation metrics"):
            self.run_provisioning(missing)

    def test_unsafe_paths_are_rejected_before_media_download(self):
        for name in ["../outside", "/absolute", "C:/drive", "a\\b", "a//b", "manifest.json"]:
            with self.subTest(name=name):
                manifest = copy.deepcopy(self.manifest)
                manifest["corpora"][0]["tracks"][0]["audio"]["path"] = name
                with self.assertRaises(ValueError):
                    self.run_provisioning(manifest)

    def test_download_failure_preserves_existing_verified_bytes(self):
        self.run_provisioning(self.manifest)
        with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
            self.run_provisioning(self.manifest, content=b"corrupted")
        self.assertEqual((self.root / "ballroom-beats/audio.wav").read_bytes(), self.content)


if __name__ == "__main__":
    unittest.main()
