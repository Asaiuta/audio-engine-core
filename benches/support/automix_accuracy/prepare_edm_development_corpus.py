"""Prepare the GiantSteps MTG previews with Schreiber 2018 tapped tempi for EDM development.

Development diagnostics only. The corpus is disjoint from the frozen
GiantSteps/Ballroom evaluation by Beatport ID and by authors' MD5, and is
split once, before any prediction, into a fit half and a validation half by
recording-ID hash parity. Constants may be selected on the fit half only.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import http.client
import json
import math
from pathlib import Path
import re
import stat
import zipfile

from prepare_public_corpus import (
    fetch, file_digest, inspect_members, read_annotations, write_json,
)
from provision_corpus import checked_path

HERE = Path(__file__).resolve().parent
ANNOTATION_REVISION = "fd7b8c584f7bd6d720d170c325a6d42c9bf75a6b"
ANNOTATION_URL = ("https://codeload.github.com/GiantSteps/giantsteps-mtg-key-dataset/zip/"
                  + ANNOTATION_REVISION)
ANNOTATION_SHA256 = "c754c3fabbc0bf275d399028e212fa2c9c6680084bd003e3a283ae7b3ea22996"
LABEL_URL = "https://www.tagtraum.com/download/schreiber_tempo_cnn_ismir2018.zip"
LABEL_SHA256 = "554009d7f57e4c4ae7a6f44bf547261aef5bec38ef4263b81bd3ac3a617ab080"
LABEL_MEMBER = "giantsteps-mtg-tempo.tsv"
AUDIO_URL = "https://www.cp.jku.at/datasets/giantsteps/mtg_key_backup/"
MAX_AUDIO_BYTES = 8_000_000
NORMALIZATION_REVISION = "giantsteps-mtg-tempo-development-v1"
DATASET_ID = "giantsteps-mtg"
CORPUS_PREFIX = "giantsteps-mtg-tempo-development"
HALVES = ("fit", "validation")
EXPECTED_SOURCE_COUNT = 1486
EXPECTED_LABEL_COUNT = 1159
LABEL_CAVEAT = ("Schreiber and Mueller 2018 training annotations: single-annotator integer BPM, "
                "marked unverified by tempo_eval; development diagnostics only")


def parse_labels(text):
    labels = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        fields = line.split("\t")
        if len(fields) != 4 or not re.fullmatch(r"[0-9]+", fields[0]) or fields[0] in labels:
            raise ValueError("MTG tempo label rows require unique numeric ID, BPM, KEY, GENRE")
        bpm = float(fields[1])
        if not math.isfinite(bpm) or bpm < 0:
            raise ValueError(f"MTG tempo label must be finite and nonnegative: {fields[0]}")
        labels[fields[0]] = {"bpm": bpm, "key": fields[2], "genre": fields[3]}
    if len(labels) != EXPECTED_LABEL_COUNT:
        raise ValueError("unexpected MTG tempo label inventory")
    return labels


def read_label_archive(path):
    with zipfile.ZipFile(path) as archive:
        infos = archive.infolist()
        inspection = inspect_members([
            (info.filename, info.file_size,
             stat.S_IFMT(info.external_attr >> 16) in {0, stat.S_IFREG, stat.S_IFDIR})
            for info in infos
        ], 100, 5_000_000)
        return archive.read(LABEL_MEMBER).decode("utf-8"), inspection


def parse_metadata(text):
    lines = text.splitlines()
    if not lines or lines[0].split("\t") != ["ID", "ARTIST", "SONG TITLE", "MIX", "LABEL",
                                              "BP GENRE", "BP BPM", "BP KEY"]:
        raise ValueError("unexpected Beatport metadata header")
    rows = {}
    for line in lines[1:]:
        fields = line.split("\t")
        if len(fields) != 8 or fields[0] in rows:
            raise ValueError("malformed or duplicate Beatport metadata row")
        rows[fields[0]] = {"beatport_genre": fields[5], "beatport_bpm": fields[6] or None}
    return rows


def half_of(recording_id):
    """Deterministic fit/validation assignment fixed before any prediction."""
    digest = hashlib.sha256(recording_id.encode("utf-8")).hexdigest()
    return HALVES[int(digest[:2], 16) % 2]


def plan_selection(labels, md5):
    """Return (included ids, exclusions) using only pre-declared rules."""
    missing = sorted(set(labels) - set(md5))
    if missing:
        raise ValueError(f"labelled tracks without authors' MD5: {missing[:5]}")
    exclusions, owners = [], {}
    included = []
    for track_id in sorted(labels):
        recording_id = f"{DATASET_ID}:{track_id}"
        if labels[track_id]["bpm"] <= 0:
            exclusions.append({"recording_id": recording_id,
                               "reason": "label BPM is 0 (no tapped tempo)"})
            continue
        digest = md5[track_id]
        if digest in owners:
            exclusions.append({"recording_id": recording_id,
                               "reason": f"exact authors' MD5 duplicate of {DATASET_ID}:{owners[digest]}"})
            continue
        owners[digest] = track_id
        included.append(track_id)
    return included, exclusions


def check_disjoint(md5, evaluation, frozen_md5):
    forbidden_ids = {track["recording_id"].split(":", 1)[1]
                     for corpus in evaluation["corpora"] for track in corpus["tracks"]
                     if track["recording_id"].startswith("giantsteps:")}
    forbidden_ids |= {entry["recording_id"].split(":", 1)[1]
                      for corpus in evaluation["corpora"] for entry in corpus["exclusions"]
                      if entry["recording_id"].startswith("giantsteps:")}
    shared_ids = sorted(set(md5) & forbidden_ids)
    shared_md5 = sorted(track_id for track_id, digest in md5.items() if digest in frozen_md5)
    if shared_ids or shared_md5:
        raise ValueError(f"frozen evaluation overlap: ids {shared_ids[:5]} md5 {shared_md5[:5]}")
    return {"frozen_giantsteps_ids": len(forbidden_ids), "frozen_giantsteps_md5": len(frozen_md5),
            "shared_ids": 0, "shared_md5": 0}


def frozen_md5_set(evaluation_sources):
    archive = evaluation_sources / "giantsteps-tempo.zip"
    if not archive.exists():
        return None
    files, _ = read_annotations(archive)
    return {value.split()[0] for name, value in files.items()
            if name.startswith("md5/") and name.endswith(".md5")}


def prepare(root, source_cache, evaluation_manifest, evaluation_sources, workers):
    root, source_cache = Path(root).resolve(), Path(source_cache).resolve()
    evaluation_manifest = Path(evaluation_manifest).resolve()
    evaluation_root = evaluation_manifest.parent
    if root.is_relative_to(evaluation_root) or evaluation_root.is_relative_to(root):
        raise ValueError("development output must be disjoint from frozen evaluation data")
    expected = (HERE / "public_corpus.sha256").read_text(encoding="ascii").split()[0]
    if file_digest(evaluation_manifest) != expected:
        raise ValueError("frozen evaluation manifest SHA-256 mismatch")
    evaluation = json.loads(evaluation_manifest.read_text(encoding="utf-8"))
    frozen_md5 = frozen_md5_set(Path(evaluation_sources).resolve())
    if frozen_md5 is None:
        raise ValueError("frozen GiantSteps source archive is required for the MD5 disjointness proof")
    source_cache.mkdir(parents=True, exist_ok=True)
    archive_path = checked_path(source_cache, "giantsteps-mtg-key.zip")
    fetch(ANNOTATION_URL, archive_path, ANNOTATION_SHA256, 30_000_000)
    with zipfile.ZipFile(archive_path) as archive:
        if archive.comment.decode("ascii") != ANNOTATION_REVISION:
            raise ValueError("annotation archive revision mismatch")
    files, inspection = read_annotations(archive_path)
    md5 = {name.removeprefix("md5/").removesuffix(".LOFI.md5"): value.split()[0]
           for name, value in files.items() if name.startswith("md5/") and name.endswith(".md5")}
    if len(md5) != EXPECTED_SOURCE_COUNT or not all(
            re.fullmatch(r"[0-9]+", key) and re.fullmatch(r"[0-9a-f]{32}", value)
            for key, value in md5.items()):
        raise ValueError("unexpected MTG key source inventory")
    metadata = parse_metadata(files["annotations/beatport_metadata.txt"])
    label_path = checked_path(source_cache, "schreiber_tempo_cnn_ismir2018.zip")
    fetch(LABEL_URL, label_path, LABEL_SHA256, 5_000_000)
    label_text, label_inspection = read_label_archive(label_path)
    labels = parse_labels(label_text)
    disjoint = check_disjoint(md5, evaluation, frozen_md5)
    included, exclusions = plan_selection(labels, md5)
    source_hashes = {name: file_digest(HERE / name) for name in [
        "prepare_edm_development_corpus.py", "prepare_public_corpus.py", "provision_corpus.py",
    ]}
    normalization = NORMALIZATION_REVISION + ":sha256:" + hashlib.sha256(
        json.dumps(source_hashes, sort_keys=True).encode()).hexdigest()
    halves = {track_id: half_of(f"{DATASET_ID}:{track_id}") for track_id in included}
    excluded_halves = {entry["recording_id"]: half_of(entry["recording_id"]) for entry in exclusions}
    root.mkdir(parents=True, exist_ok=True)
    write_json(root / "development-plan.json", {
        "schema_version": 1, "split": "development",
        "annotation_revision": ANNOTATION_REVISION, "annotation_sha256": ANNOTATION_SHA256,
        "label_url": LABEL_URL, "label_sha256": LABEL_SHA256, "label_member": LABEL_MEMBER,
        "audio_url": AUDIO_URL, "evaluation_manifest_sha256": expected,
        "normalizer_sources_sha256": source_hashes, "normalization_revision": normalization,
        "selection": ("all labelled rows with positive BPM; exact authors' MD5 duplicates keep the "
                      "lexicographically first ID; no genre or tempo filtering"),
        "half_rule": "sha256(recording_id) first byte even -> fit, odd -> validation; fixed before any prediction",
        "fit_rule": "fitted constants may be selected on the fit half only; reported scores come from the validation half",
        "label_caveat": LABEL_CAVEAT,
        "disjointness": disjoint,
        "included": {half: sorted(t for t in included if halves[t] == half) for half in HALVES},
        "exclusions": [entry | {"half": excluded_halves[entry["recording_id"]]} for entry in exclusions],
        "evaluation_policy": "development diagnostics only; do not replace the frozen GiantSteps/Ballroom evaluation",
    })

    # Resolve and validate every destination serially before any worker runs;
    # concurrent Path.resolve() during sibling temp-file replacement was
    # observed to misreport containment on Windows.
    destinations = {}
    for track_id in included:
        audio_relative = f"audio/{CORPUS_PREFIX}/{track_id}.LOFI.mp3"
        annotation_relative = f"annotations/{CORPUS_PREFIX}/{track_id}.json"
        destinations[track_id] = (audio_relative, checked_path(root, audio_relative),
                                  annotation_relative, checked_path(root, annotation_relative))

    def prepare_track(track_id):
        audio_relative, audio_path, annotation_relative, annotation_path = destinations[track_id]
        fetch(AUDIO_URL + f"{track_id}.LOFI.mp3", audio_path, md5[track_id], MAX_AUDIO_BYTES, "md5")
        write_json(annotation_path, {"schema_version": 1, "tempo_bpm": labels[track_id]["bpm"]})
        return {"id": track_id, "recording_id": f"{DATASET_ID}:{track_id}", "split": "development",
                "audio": {"path": audio_relative, "sha256": file_digest(audio_path)},
                "annotation": {"path": annotation_relative, "sha256": file_digest(annotation_path)}}

    tracks, failures = {}, []
    with ThreadPoolExecutor(max_workers=workers) as pool:
        jobs = {pool.submit(prepare_track, track_id): track_id for track_id in included}
        for job in as_completed(jobs):
            try:
                track = job.result()
                tracks[track["id"]] = track
                if len(tracks) % 50 == 0 or len(tracks) == len(included):
                    print(f"Verified MTG previews: {len(tracks)}/{len(included)}", flush=True)
            except Exception as error:
                failures.append(f"{jobs[job]}: {error}")
    write_json(root / "preparation-status.json", {"verified_count": len(tracks),
                                                 "failures": sorted(failures)})
    if failures:
        raise ValueError("incomplete development corpus: " + "; ".join(sorted(failures)))
    corpora = []
    for half in HALVES:
        half_tracks = [tracks[t] for t in sorted(included) if halves[t] == half]
        half_exclusions = [{"recording_id": e["recording_id"], "reason": e["reason"]}
                           for e in exclusions if excluded_halves[e["recording_id"]] == half]
        corpora.append({
            "id": f"{CORPUS_PREFIX}-{half}",
            "dataset_name": "GiantSteps MTG Key previews with Schreiber 2018 tapped tempi",
            "dataset_version": f"MTG key dataset {ANNOTATION_REVISION[:12]}; tapped tempo subset ISMIR 2018",
            "source_revision": f"annotations:{ANNOTATION_REVISION};labels:sha256:{LABEL_SHA256[:16]}",
            "source_url": "https://github.com/GiantSteps/giantsteps-mtg-key-dataset",
            "license_note": ("Beatport previews from authors' JKU mirror; no repository license; "
                             "local research only, do not redistribute; " + LABEL_CAVEAT),
            "annotation_format": "automix_annotation_v1", "normalization_revision": normalization,
            "split_id": f"{CORPUS_PREFIX}-{half}-hashparity-v1",
            "expected_track_count": len(half_tracks) + len(half_exclusions),
            "metrics": ["tempo_accuracy1", "tempo_accuracy2"],
            "tracks": half_tracks, "exclusions": half_exclusions,
        })
    write_json(root / "manifest.json", {"schema_version": 1, "corpora": corpora})
    write_json(root / "provenance.json", {
        "schema_version": 1, "split": "development", "normalization_revision": normalization,
        "manifest_sha256": file_digest(root / "manifest.json"),
        "development_plan_sha256": file_digest(root / "development-plan.json"),
        "evaluation_manifest_sha256": expected,
        "annotation_archive_inspection": inspection, "label_archive_inspection": label_inspection,
        "included_count": len(included), "excluded_count": len(exclusions),
        "half_counts": {half: sum(1 for t in included if halves[t] == half) for half in HALVES},
        "labels": {t: labels[t] | metadata.get(t, {}) for t in sorted(labels)},
        "beatport_metadata_policy": "beatport_bpm is algorithmic vendor metadata, never a scored reference",
        "overlap_check": "zero shared Beatport IDs or authors' MD5 with the pinned frozen GiantSteps set; within-development exact MD5 duplicates removed",
        "overlap_limit": "hashes do not establish absence of differently encoded or offset excerpts of the same recording",
    })
    print(f"Prepared {len(included)} development tracks in two halves, {len(exclusions)} explicit exclusions", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", default="target/automix-edm-development-data")
    parser.add_argument("--source-cache", default="target/automix-edm-development-sources")
    parser.add_argument("--evaluation-manifest", default="target/automix-corpus-data/manifest.json")
    parser.add_argument("--evaluation-sources", default="target/automix-corpus-sources")
    parser.add_argument("--workers", type=int, choices=range(1, 9), default=4)
    args = parser.parse_args()
    try:
        prepare(args.out, args.source_cache, args.evaluation_manifest, args.evaluation_sources,
                args.workers)
    except (OSError, ValueError, KeyError, http.client.HTTPException, zipfile.BadZipFile) as error:
        parser.exit(1, f"EDM development corpus preparation failed: {error}\n")


if __name__ == "__main__":
    main()
