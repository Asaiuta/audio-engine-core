import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import prepare_edm_development_corpus as edm


class EdmDevelopmentCorpusTests(unittest.TestCase):
    def test_label_rows_require_unique_numeric_ids_and_finite_bpm(self):
        labels = edm.parse_labels("\n".join(
            [f"{i}\t{120 + i}\tAm\tHouse" for i in range(edm.EXPECTED_LABEL_COUNT)]))
        self.assertEqual(labels["3"], {"bpm": 123.0, "key": "Am", "genre": "House"})
        for bad in ["1\t120\tAm", "x\t120\tAm\tHouse", "1\tnan\tAm\tHouse", "1\t-1\tAm\tHouse",
                    "1\t120\tAm\tHouse\n1\t121\tAm\tHouse"]:
            with self.assertRaises(ValueError):
                edm.parse_labels(bad)

    def test_half_assignment_is_deterministic_and_prediction_free(self):
        self.assertEqual(edm.half_of("giantsteps-mtg:5061"), edm.half_of("giantsteps-mtg:5061"))
        self.assertIn(edm.half_of("giantsteps-mtg:5061"), edm.HALVES)
        halves = {edm.half_of(f"giantsteps-mtg:{i}") for i in range(200)}
        self.assertEqual(halves, set(edm.HALVES))

    def test_selection_excludes_zero_bpm_and_later_md5_duplicates_only(self):
        labels = {"10": {"bpm": 128.0}, "2": {"bpm": 0.0}, "3": {"bpm": 90.0}, "30": {"bpm": 91.0}}
        md5 = {"10": "a" * 32, "2": "b" * 32, "3": "c" * 32, "30": "a" * 32, "99": "d" * 32}
        included, exclusions = edm.plan_selection(labels, md5)
        self.assertEqual(included, ["10", "3"])
        self.assertEqual(exclusions, [
            {"recording_id": "giantsteps-mtg:2", "reason": "label BPM is 0 (no tapped tempo)"},
            {"recording_id": "giantsteps-mtg:30",
             "reason": "exact authors' MD5 duplicate of giantsteps-mtg:10"},
        ])
        with self.assertRaisesRegex(ValueError, "without authors' MD5"):
            edm.plan_selection({"7": {"bpm": 120.0}}, md5)

    def test_frozen_overlap_by_id_or_md5_fails(self):
        evaluation = {"corpora": [{"tracks": [{"recording_id": "giantsteps:5"}],
                                   "exclusions": [{"recording_id": "giantsteps:6"}]}]}
        md5 = {"1": "a" * 32}
        self.assertEqual(edm.check_disjoint(md5, evaluation, {"z" * 32})["shared_ids"], 0)
        for overlap in [{"5": "b" * 32}, {"6": "b" * 32}, {"1": "z" * 32}]:
            with self.assertRaisesRegex(ValueError, "frozen evaluation overlap"):
                edm.check_disjoint(overlap, evaluation, {"z" * 32})

    def test_changed_evaluation_lock_fails_before_network_or_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            evaluation = root / "evaluation" / "manifest.json"
            evaluation.parent.mkdir()
            evaluation.write_text(json.dumps({"corpora": []}), encoding="utf-8")
            with patch.object(edm, "fetch") as network:
                with self.assertRaisesRegex(ValueError, "evaluation manifest SHA-256 mismatch"):
                    edm.prepare(root / "development", root / "sources", evaluation,
                                root / "sources", 1)
                network.assert_not_called()
            self.assertFalse((root / "development").exists())


if __name__ == "__main__":
    unittest.main()
