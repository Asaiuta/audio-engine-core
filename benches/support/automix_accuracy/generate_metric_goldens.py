"""Regenerate independent metric oracles with mir_eval==0.8.2.

Only this developer utility needs Python/NumPy/SciPy/mir_eval. The benchmark
and tests consume the committed JSON and stay Rust/offline. Example:
  python generate_metric_goldens.py --reference-root .tmp/automix-metric-reference
"""

import argparse
import json
import sys
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--reference-root", type=Path)
args = parser.parse_args()
if args.reference_root:
    sys.path.insert(0, str(args.reference_root.resolve()))

import mir_eval  # noqa: E402
import numpy as np  # noqa: E402

assert mir_eval.__version__ == "0.8.2", mir_eval.__version__

reference = np.arange(0.0, 20.0, 0.5)
beat_cases = []
patterns = {
    "exact": reference,
    "shifted_40ms": reference + 0.04,
    "shifted_90ms": reference + 0.09,
    "offbeat": reference + 0.25,
    "half": reference[::2],
    "half_odd": reference[1::2],
    "double": np.arange(0.0, 20.0, 0.25),
    "triple": np.arange(0.0, 20.0, 1.0 / 6.0),
    "missing": reference[::3],
    "no_prediction": np.array([]),
    "crowded": np.sort(np.concatenate((reference + 0.02, reference + 0.03))),
    "trim_and_interval": np.array([0.0, 4.99, 5.0, 5.5, 6.0, 19.5, 20.0, 21.0]),
    "period_drift": np.arange(0.0, 20.0, 0.56),
}
for name, prediction in patterns.items():
    trimmed_reference = mir_eval.beat.trim_beats(reference[reference < 20.0])
    trimmed_prediction = mir_eval.beat.trim_beats(prediction[prediction < 20.0])
    beat_cases.append({
        "name": name,
        "start_sec": 0.0,
        "end_sec": 20.0,
        "reference": reference.tolist(),
        "prediction": prediction.tolist(),
        "f_measure": mir_eval.beat.f_measure(trimmed_reference, trimmed_prediction, 0.07),
        "amlt": mir_eval.beat.continuity(trimmed_reference, trimmed_prediction, 0.175, 0.175)[3],
    })

# Short, tied-nearest and inclusive-collar cases supplement the regular grid.
for name, ref, pred in [
    ("single", [5.0], [5.0]),
    ("nearest_tie", [5.0, 6.0, 7.0, 8.0], [5.5, 6.5, 7.5]),
    ("collar_boundary", [0.0, 1.0], [0.07, 1.07]),
    ("one_to_one", [5.0, 5.06, 6.0], [5.03, 6.0]),
]:
    ref, pred = np.array(ref), np.array(pred)
    beat_cases.append({
        "name": name,
        "start_sec": 0.0,
        "end_sec": 20.0,
        "already_trimmed": True,
        "reference": ref.tolist(),
        "prediction": pred.tolist(),
        "f_measure": mir_eval.beat.f_measure(ref, pred, 0.07),
        "amlt": mir_eval.beat.continuity(ref, pred, 0.175, 0.175)[3],
    })

names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]
key_cases = []
for tonic in range(12):
    for mode in ("major", "minor"):
        for estimate_tonic in range(12):
            for estimate_mode in ("major", "minor"):
                ref = f"{names[tonic]} {mode}"
                pred = f"{names[estimate_tonic]} {estimate_mode}"
                key_cases.append({
                    "reference": {"pitch_class": tonic, "mode": mode},
                    "prediction": {"pitch_class": estimate_tonic, "mode": estimate_mode},
                    "exact": float(ref == pred),
                    "weighted": mir_eval.key.weighted_score(ref, pred),
                })
        key_cases.append({
            "reference": {"pitch_class": tonic, "mode": mode},
            "prediction": None,
            "exact": 0.0,
            "weighted": mir_eval.key.weighted_score(f"{names[tonic]} {mode}", "X"),
        })

output = {
    "reference": "mir_eval==0.8.2",
    "protocol": "head [0,20), trim both beat arrays at >=5 s except already_trimmed boundary cases",
    "beat_cases": beat_cases,
    "key_cases": key_cases,
}
tempo_cases = []
for reference_bpm in (120.0, 127.3):
    for prediction_bpm in (40.0, 60.0, 80.0, 120.0, 124.7, 125.0, 127.3, 240.0, 360.0, None):
        def hit(multiplier):
            if prediction_bpm is None:
                return 0.0
            # Duplicate the single primary label/prediction for mir_eval's
            # two-tempo interface; no prediction-dependent primary selection.
            return float(mir_eval.tempo.detection(
                np.array([reference_bpm * multiplier] * 2), 0.5,
                np.array([prediction_bpm] * 2), tol=0.04,
            )[1])
        tempo_cases.append({
            "reference": reference_bpm, "prediction": prediction_bpm,
            "accuracy1": hit(1.0),
            "accuracy2": max(hit(factor) for factor in (1/3, 1/2, 1, 2, 3)),
        })
output["tempo_cases"] = tempo_cases
Path(__file__).with_name("metric_goldens.json").write_text(
    json.dumps(output, indent=2, allow_nan=False) + "\n", encoding="utf-8"
)
