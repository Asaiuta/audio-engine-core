"""Prepare the pinned ISMIR GTZAN mini recordings for development, never acceptance."""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import http.client
import io
import json
import math
from pathlib import Path, PurePosixPath
import re
import tempfile
import time
from urllib.request import urlopen
import wave
import zipfile

from prepare_public_corpus import (
    fetch, file_digest, input_file, normalize_beats, read_annotations, write_json,
)
from provision_corpus import checked_path, checked_url


HERE = Path(__file__).resolve().parent
AUDIO_REVISION = "a61439e86c13037011fde8e0f0743ec55c50bce3"
ANNOTATION_REVISION = "fd9cafdebb6426fee41c3718791be61306323dd7"
ANNOTATION_SHA256 = "649eede047f9d8fdfdc87897748749b18b7ecf191967ed61f042e4a88f43ca0d"
ANNOTATION_URL = ("https://codeload.github.com/TempoBeatDownbeat/gtzan_tempo_beat/zip/"
                  + ANNOTATION_REVISION)
NORMALIZATION_REVISION = "gtzan-mini-development-v1"
CORPUS_ID = "gtzan-mini-development"
MAX_AUDIO_BYTES = 4_000_000


def git_blob_digest(data):
    return hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()


def fetch_audio(url, destination, expected_blob):
    """Verify the pinned Git object before publishing any original WAV bytes."""
    checked_url(url)
    if not re.fullmatch(r"[0-9a-f]{40}", expected_blob):
        raise ValueError("expected a pinned Git blob SHA-1")
    if destination.exists():
        if (destination.stat().st_size > MAX_AUDIO_BYTES
                or git_blob_digest(destination.read_bytes()) != expected_blob):
            raise ValueError(f"cached audio Git blob mismatch: {destination}")
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    for attempt in range(3):
        try:
            with urlopen(url, timeout=30) as response:
                checked_url(response.geturl())
                data = response.read(MAX_AUDIO_BYTES + 1)
            break
        except (OSError, http.client.HTTPException):
            if attempt == 2:
                raise
            time.sleep(2 ** attempt)
    if len(data) > MAX_AUDIO_BYTES:
        raise ValueError(f"audio exceeds size limit: {destination.name}")
    if git_blob_digest(data) != expected_blob:
        raise ValueError(f"Git blob mismatch: {destination.name}")
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
            temporary = Path(output.name)
            output.write(data)
        temporary.replace(destination)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()


def source_inventory():
    inventory = json.loads((HERE / "gtzan_mini_sources.json").read_text(encoding="utf-8"))
    if (inventory.get("schema_version") != 1 or inventory.get("revision") != AUDIO_REVISION
            or len(inventory["files"]) != 100):
        raise ValueError("unexpected pinned GTZAN mini source inventory")
    paths = set()
    for entry in inventory["files"]:
        match = re.fullmatch(r"genres/([a-z]+)/\1\.([0-9]{5})\.wav", entry["path"])
        if (not match or entry["path"] in paths
                or not re.fullmatch(r"[0-9a-f]{40}", entry["git_blob_sha1"])):
            raise ValueError("invalid or duplicate GTZAN audio identity")
        paths.add(entry["path"])
    return sorted(inventory["files"], key=lambda item: item["path"])


def normalize_annotation(tempo_text, beat_text):
    values = tempo_text.split()
    if len(values) != 1:
        raise ValueError("GTZAN tempo requires one native BPM label")
    tempo = float(values[0])
    if not math.isfinite(tempo) or tempo <= 0:
        raise ValueError("GTZAN tempo must be positive and finite")
    annotation = normalize_beats(beat_text)
    annotation["tempo_bpm"] = tempo
    return annotation


def validate_audio(path, annotation):
    data = path.read_bytes()
    with wave.open(io.BytesIO(data), "rb") as audio:
        rate, channels, width, frames = (audio.getframerate(), audio.getnchannels(),
                                        audio.getsampwidth(), audio.getnframes())
        pcm = audio.readframes(frames)
    if rate <= 0 or frames <= 0 or len(pcm) != frames * channels * width:
        raise ValueError(f"invalid native PCM coverage: {path.name}")
    end = min(60.0, frames / rate)
    if sum(5 <= beat < end for beat in annotation["beats_sec"]) < 2:
        raise ValueError(f"insufficient native beat coverage: {path.name}")
    # A format-qualified PCM hash catches exact audio in different WAV wrappers.
    pcm_hash = hashlib.sha256(f"{rate}:{channels}:{width}:".encode() + pcm).hexdigest()
    return {"sample_rate_hz": rate, "channels": channels, "frames": frames,
            "duration_sec": frames / rate, "pcm_sha256": pcm_hash}


def select_unique(tracks, audio_details, evaluation):
    forbidden_hashes = {track["audio"]["sha256"]
                        for corpus in evaluation["corpora"] for track in corpus["tracks"]}
    forbidden_ids = {track["recording_id"]
                     for corpus in evaluation["corpora"] for track in corpus["tracks"]}
    kept, exclusions, pcm_owners = [], [], {}
    for track in sorted(tracks, key=lambda item: item["id"]):
        if (track["audio"]["sha256"] in forbidden_hashes
                or track["recording_id"] in forbidden_ids):
            raise ValueError(f"frozen evaluation overlap: {track['id']}")
        pcm_hash = audio_details[track["id"]]["pcm_sha256"]
        if pcm_hash in pcm_owners:
            exclusions.append({"recording_id": track["recording_id"],
                               "reason": f"exact native PCM duplicate of {pcm_owners[pcm_hash]}"})
        else:
            pcm_owners[pcm_hash] = track["id"]
            kept.append(track)
    return kept, exclusions


def prepare(root, source_cache, evaluation_manifest, workers):
    root, source_cache = Path(root).resolve(), Path(source_cache).resolve()
    evaluation_manifest = Path(evaluation_manifest).resolve()
    evaluation_root = evaluation_manifest.parent
    if root.is_relative_to(evaluation_root) or evaluation_root.is_relative_to(root):
        raise ValueError("development output must be disjoint from frozen evaluation data")
    expected = (HERE / "public_corpus.sha256").read_text(encoding="ascii").split()[0]
    if file_digest(evaluation_manifest) != expected:
        raise ValueError("frozen evaluation manifest SHA-256 mismatch")
    evaluation = json.loads(evaluation_manifest.read_text(encoding="utf-8"))
    inventory = source_inventory()
    source_cache.mkdir(parents=True, exist_ok=True)
    archive_path = checked_path(source_cache, "gtzan-annotations.zip")
    fetch(ANNOTATION_URL, archive_path, ANNOTATION_SHA256, 10_000_000)
    with zipfile.ZipFile(archive_path) as archive:
        if archive.comment.decode("ascii") != ANNOTATION_REVISION:
            raise ValueError("annotation archive revision mismatch")
    annotations, inspection = read_annotations(archive_path)
    source_hashes = {name: file_digest(HERE / name) for name in [
        "prepare_development_corpus.py", "gtzan_mini_sources.json",
        "prepare_public_corpus.py", "provision_corpus.py",
    ]}
    normalization = NORMALIZATION_REVISION + ":sha256:" + hashlib.sha256(
        json.dumps(source_hashes, sort_keys=True).encode()).hexdigest()
    root.mkdir(parents=True, exist_ok=True)
    write_json(root / "development-plan.json", {
        "schema_version": 1, "split": "development", "audio_revision": AUDIO_REVISION,
        "annotation_revision": ANNOTATION_REVISION, "annotation_sha256": ANNOTATION_SHA256,
        "evaluation_manifest_sha256": expected, "normalizer_sources_sha256": source_hashes,
        "selection": "all 100 source WAVs, exact native PCM duplicates keep lexical first; no score/genre/tempo filtering",
        "sources": inventory, "normalization_revision": normalization,
        "evaluation_policy": "development diagnostics only; do not replace the frozen GiantSteps/Ballroom evaluation",
    })

    def prepare_track(entry):
        track_id = PurePosixPath(entry["path"]).stem
        label = "gtzan_" + track_id.replace(".", "_")
        annotation = normalize_annotation(annotations[f"tempo/{label}.bpm"],
                                          annotations[f"beats/{label}.beats"])
        audio_relative = f"audio/{CORPUS_ID}/{track_id}.wav"
        audio_path = checked_path(root, audio_relative)
        url = f"https://raw.githubusercontent.com/TempoBeatDownbeat/gtzan_mini/{AUDIO_REVISION}/{entry['path']}"
        fetch_audio(url, audio_path, entry["git_blob_sha1"])
        details = validate_audio(audio_path, annotation)
        annotation_relative = f"annotations/{CORPUS_ID}/{track_id}.json"
        write_json(checked_path(root, annotation_relative), annotation)
        return ({"id": track_id, "recording_id": f"gtzan:{track_id}", "split": "development",
                 "audio": input_file(root, audio_relative),
                 "annotation": input_file(root, annotation_relative)}, details)

    tracks, details, failures = [], {}, []
    with ThreadPoolExecutor(max_workers=workers) as pool:
        jobs = {pool.submit(prepare_track, entry): entry["path"] for entry in inventory}
        for job in as_completed(jobs):
            try:
                track, audio = job.result()
                tracks.append(track)
                details[track["id"]] = audio
                if len(tracks) % 10 == 0:
                    print(f"Verified GTZAN mini: {len(tracks)}/100", flush=True)
            except Exception as error:
                failures.append(f"{jobs[job]}: {error}")
    write_json(root / "preparation-status.json", {"verified_count": len(tracks),
                                                 "failures": sorted(failures)})
    if failures:
        raise ValueError("incomplete development corpus: " + "; ".join(sorted(failures)))
    included, exclusions = select_unique(tracks, details, evaluation)
    corpus = {
        "id": CORPUS_ID, "dataset_name": "GTZAN mini (ISMIR 2021 tutorial)",
        "dataset_version": "pinned 100-track mini", "source_revision": f"audio:{AUDIO_REVISION};annotations:{ANNOTATION_REVISION}",
        "source_url": "https://github.com/TempoBeatDownbeat/gtzan_mini",
        "license_note": "Authors' public ISMIR research tutorial data; no standalone audio license found; local research only, do not redistribute",
        "annotation_format": "automix_annotation_v1", "normalization_revision": normalization,
        "split_id": "gtzan-mini-all-development-v1", "expected_track_count": 100,
        "metrics": ["tempo_accuracy1", "tempo_accuracy2", "beat_f_measure", "beat_amlt"],
        "tracks": included, "exclusions": exclusions,
    }
    write_json(root / "manifest.json", {"schema_version": 1, "corpora": [corpus]})
    write_json(root / "provenance.json", {
        "schema_version": 1, "split": "development", "normalization_revision": normalization,
        "manifest_sha256": file_digest(root / "manifest.json"),
        "development_plan_sha256": file_digest(root / "development-plan.json"),
        "evaluation_manifest_sha256": expected, "annotation_archive_inspection": inspection,
        "included_count": len(included), "excluded_count": len(exclusions), "audio": details,
        "overlap_check": "zero shared recording IDs or original-file SHA-256 with pinned evaluation; within-development exact PCM duplicates removed",
        "overlap_limit": "hashes do not establish absence of differently encoded or offset excerpts of the same recording",
    })
    print(f"Prepared {len(included)} development tracks, {len(exclusions)} explicit exclusions", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", default="target/automix-development-data")
    parser.add_argument("--source-cache", default="target/automix-development-sources")
    parser.add_argument("--evaluation-manifest", default="target/automix-corpus-data/manifest.json")
    parser.add_argument("--workers", type=int, choices=range(1, 9), default=4)
    args = parser.parse_args()
    prepare(args.out, args.source_cache, args.evaluation_manifest, args.workers)


if __name__ == "__main__":
    main()
