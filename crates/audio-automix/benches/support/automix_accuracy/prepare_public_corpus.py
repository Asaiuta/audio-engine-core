"""Prepare pinned GiantSteps tempo and deduplicated Ballroom beat evaluation data."""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import http.client
import json
import math
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import tarfile
import tempfile
import time
import zipfile

from provision_corpus import REQUIRED_METRICS, checked_path, download


GIANTSTEPS_REVISION = "d51ab2422e76abacfaa86616a57054bc222ec9fd"
BALLROOM_REVISION = "1db08914a8ae15edb01f104046e30bad88effe67"
NORMALIZATION_REVISION = "official-tempo-beats-v1"
GIANTSTEPS_NO_TEMPO = {"1327052", "3041381", "3041383"}
GIANTSTEPS_AUDIO = "https://www.cp.jku.at/datasets/giantsteps/backup/"
SOURCES = {
    "giantsteps-tempo.zip": {
        "url": "https://codeload.github.com/GiantSteps/giantsteps-tempo-dataset/zip/"
        + GIANTSTEPS_REVISION,
        "sha256": "05b908c799be0e616e0632d7019228fc0e424c106c1873c822fa12957d7f0a87",
        "max_bytes": 10_000_000,
    },
    "ballroom-annotations.zip": {
        "url": "https://codeload.github.com/CPJKU/BallroomAnnotations/zip/"
        + BALLROOM_REVISION,
        "sha256": "7c6882c51f7ce0d76679d4b8d6511ad068219890b80194451f880e89671bc31b",
        "max_bytes": 10_000_000,
    },
    "ballroom-data1.tar.gz": {
        "url": "https://mtg.upf.edu/ismir2004/contest/tempoContest/data1.tar.gz",
        "sha256": "e2f85f5ac230523498ca3b9addf225de19f1d4d1cb7d8724a40bc0044b7c5947",
        "upstream_md5": "2872a3e52070bc342a4510a95e2fa0b8",
        "max_bytes": 1_453_888_083,
    },
}


def file_digest(path, algorithm="sha256"):
    with path.open("rb") as source:
        return hashlib.file_digest(source, algorithm).hexdigest()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as output:
            temporary = Path(output.name)
            output.write((json.dumps(value, indent=2, sort_keys=True, allow_nan=False)
                          + "\n").encode("utf-8"))
        temporary.replace(path)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()


def fetch(url, destination, expected, max_bytes, algorithm="sha256"):
    if destination.exists():
        if destination.stat().st_size > max_bytes or file_digest(destination, algorithm) != expected:
            raise ValueError(f"cached source checksum mismatch: {destination}")
        return
    for attempt in range(3):
        try:
            download(url, destination, expected, algorithm=algorithm, max_bytes=max_bytes)
            return
        except (OSError, http.client.HTTPException):
            if attempt == 2:
                raise
            time.sleep(2 ** attempt)


def archive_name(name):
    name = name.rstrip("/")
    if (not name or "\\" in name or ":" in name
            or any(part in {"", ".", ".."} for part in name.split("/"))
            or any(ord(character) < 32 for character in name)):
        raise ValueError(f"unsafe archive path: {name!r}")
    return name


def inspect_members(members, max_entries, max_total):
    names = set()
    total = 0
    for name, size, regular in members:
        canonical = archive_name(name).casefold()
        if not regular or canonical in names or not 0 <= size <= 64_000_000:
            raise ValueError(f"invalid/duplicate archive member: {name}")
        names.add(canonical)
        total += size
        if len(names) > max_entries or total > max_total:
            raise ValueError("archive exceeds entry or expanded-size limit")
    return {"entries": len(names), "expanded_bytes": total,
            "links_or_special_files": 0, "unsafe_paths": 0}


def read_annotations(path):
    with zipfile.ZipFile(path) as archive:
        infos = archive.infolist()
        inspection = inspect_members([
            (info.filename, info.file_size,
             stat.S_IFMT(info.external_attr >> 16) in {0, stat.S_IFREG, stat.S_IFDIR})
            for info in infos
        ], 10_000, 50_000_000)
        files = {}
        roots = set()
        for info in infos:
            if info.is_dir():
                continue
            root, relative = info.filename.split("/", 1)
            roots.add(root)
            files[relative] = archive.read(info).decode("utf-8")
        if len(roots) != 1:
            raise ValueError("annotation archive must contain one source root")
    return files, inspection


def normalize_tempo(text):
    values = [float(value) for value in text.split()]
    if len(values) != 3 or not all(math.isfinite(value) for value in values):
        raise ValueError("MIREX annotation requires finite T1 T2 ST1")
    first, second, salience = values
    if first <= 0 or second < 0 or not 0 <= salience <= 1:
        raise ValueError("MIREX annotation has no valid primary tempo")
    if second == 0:
        if salience != 1:
            raise ValueError("missing secondary tempo with nonzero salience")
        selected = first
    elif salience == 0.5:
        selected = min(first, second)
    else:
        selected = first if salience > 0.5 else second
    return {"schema_version": 1, "tempo_bpm": selected}


def normalize_beats(text):
    beats = []
    for line in text.splitlines():
        if not line.strip():
            continue
        values = line.split()
        if len(values) != 2:
            raise ValueError("beat annotation requires seconds and beat ID")
        beat = float(values[0])
        if (not math.isfinite(beat) or beat < 0 or int(values[1]) <= 0
                or (beats and beat <= beats[-1])):
            raise ValueError("beat times must be finite, nonnegative and increasing")
        beats.append(beat)
    if sum(5 <= beat < 60 for beat in beats) < 2:
        raise ValueError("beat annotation cannot support the frozen evaluation interval")
    return {"schema_version": 1, "beats_sec": beats}


def ballroom_exclusions(readme):
    replicas = {}
    for first, second in re.findall(r"^\s+(\S+\.wav) matches (\S+\.wav)\s*$", readme, re.MULTILINE):
        kept, excluded = sorted([PurePosixPath(first).stem, PurePosixPath(second).stem])
        if excluded in replicas:
            raise ValueError("duplicate replica declaration")
        replicas[excluded] = kept
    if len(replicas) != 13 or set(replicas).intersection(replicas.values()):
        raise ValueError("expected the 13 independent author-declared replica pairs")
    return replicas


def input_file(root, relative):
    path = checked_path(root, relative)
    return {"path": relative, "sha256": file_digest(path)}


def track(root, dataset, track_id, extension, annotation):
    relative = f"annotations/{dataset}/{track_id}.json"
    write_json(checked_path(root, relative), annotation)
    return {
        "id": track_id,
        "recording_id": f"{dataset}:{track_id}",
        "split": "evaluation",
        "audio": input_file(root, f"audio/{dataset}/{track_id}{extension}"),
        "annotation": input_file(root, relative),
    }


def prepare_giantsteps(root, files, workers):
    annotations = {
        PurePosixPath(name).name.removesuffix(".LOFI.mirex"): value
        for name, value in files.items() if name.startswith("annotations_v2/mirex/")
    }
    if len(annotations) != 664 or not GIANTSTEPS_NO_TEMPO.issubset(annotations):
        raise ValueError("unexpected GiantSteps source inventory")
    jobs = []
    for track_id, value in sorted(annotations.items()):
        if not re.fullmatch(r"[0-9]+", track_id):
            raise ValueError("unexpected GiantSteps identity")
        if track_id in GIANTSTEPS_NO_TEMPO:
            if any(float(number) != 0 for number in value.split()[:2]):
                raise ValueError("no-tempo exclusion no longer matches native annotation")
            continue
        label = normalize_tempo(value)
        digest = files[f"md5/{track_id}.LOFI.md5"].strip()
        destination = checked_path(root, f"audio/giantsteps/{track_id}.LOFI.mp3")
        jobs.append((track_id, label, digest, destination))

    def prepare_one(job):
        track_id, label, digest, destination = job
        fetch(GIANTSTEPS_AUDIO + destination.name, destination, digest, 8_000_000, "md5")
        return track(root, "giantsteps", track_id, ".LOFI.mp3", label)

    tracks = []
    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(prepare_one, job) for job in jobs]
        for future in as_completed(futures):
            tracks.append(future.result())
            if len(tracks) % 25 == 0 or len(tracks) == len(jobs):
                print(f"GiantSteps verified: {len(tracks)}/{len(jobs)}", flush=True)
    return sorted(tracks, key=lambda item: item["id"])


def prepare_ballroom(root, source, files, exclusions):
    annotations = {PurePosixPath(name).stem: normalize_beats(value)
                   for name, value in files.items() if name.endswith(".beats")}
    if len(annotations) != 698 or not set(exclusions).union(exclusions.values()).issubset(annotations):
        raise ValueError("unexpected Ballroom annotation inventory")
    tracks = []
    with tarfile.open(source, "r:gz") as archive:
        members = archive.getmembers()
        inspection = inspect_members([
            (member.name, member.size, member.isfile() or member.isdir())
            for member in members
        ], 2_000, 2_100_000_000)
        listing = [{"path": member.name, "bytes": member.size,
                    "type": "file" if member.isfile() else "directory"} for member in members]
        write_json(checked_path(root, "ballroom-archive-listing.json"), listing)
        audio = {}
        for member in members:
            if member.isfile() and member.name.endswith(".wav"):
                track_id = PurePosixPath(member.name).stem
                if track_id in audio:
                    raise ValueError(f"duplicate Ballroom audio identity: {track_id}")
                audio[track_id] = member
        if set(audio) != set(annotations):
            raise ValueError("Ballroom audio/annotation coverage mismatch")
        # Extract only matched WAV bytes, without restoring archive paths or modes.
        for track_id, member in audio.items():
            if track_id in exclusions:
                continue
            destination = checked_path(root, f"audio/ballroom/{track_id}.wav")
            destination.parent.mkdir(parents=True, exist_ok=True)
            temporary = None
            try:
                with archive.extractfile(member) as content:
                    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
                        temporary = Path(output.name)
                        shutil.copyfileobj(content, output, 1024 * 1024)
                if temporary.stat().st_size != member.size:
                    raise ValueError(f"truncated Ballroom member: {member.name}")
                temporary.replace(destination)
            finally:
                if temporary is not None and temporary.exists():
                    temporary.unlink()
            tracks.append(track(root, "ballroom", track_id, ".wav", annotations[track_id]))
    print(f"Ballroom verified: {len(tracks)}/685", flush=True)
    return sorted(tracks, key=lambda item: item["id"]), inspection


def prepare(cache, root, workers):
    cache, root = cache.resolve(), root.resolve()
    cache.mkdir(parents=True, exist_ok=True)
    root.mkdir(parents=True, exist_ok=True)
    for name, source in SOURCES.items():
        print(f"Verifying source: {name}", flush=True)
        fetch(source["url"], checked_path(cache, name), source["sha256"], source["max_bytes"])
    giantsteps, giantsteps_inspection = read_annotations(cache / "giantsteps-tempo.zip")
    ballroom, ballroom_inspection = read_annotations(cache / "ballroom-annotations.zip")
    replicas = ballroom_exclusions(ballroom["README.md"])
    giantsteps_excluded = [{"recording_id": f"giantsteps:{track_id}",
                           "reason": "Official README: no tempo in v2 annotations"}
                          for track_id in sorted(GIANTSTEPS_NO_TEMPO)]
    ballroom_excluded = [{"recording_id": f"ballroom:{track_id}",
                         "reason": f"Author-declared replica; retain ballroom:{kept}"}
                        for track_id, kept in sorted(replicas.items())]
    converter = file_digest(Path(__file__))
    plan = {
        "schema_version": 1,
        "normalization_revision": NORMALIZATION_REVISION,
        "converter_sha256": converter,
        "sources": SOURCES,
        "giantsteps_media_url": GIANTSTEPS_AUDIO,
        "giantsteps_integrity": "Per-file MD5 from pinned authors' ZIP, then SHA-256 in manifest",
        "tempo_label_policy": "Higher salience; lower BPM on an exact tie; no inferred beats",
        "beat_label_policy": "All native beat times; no resampling, shifting or synthetic grid",
        "split_policy": "All included real recordings are evaluation; synthetic development only",
        "replica_policy": "Retain lexicographically first source stem of each author-declared pair",
        "expected_evaluation_counts": {"giantsteps-tempo": 661, "ballroom-beats": 685},
        "exclusions": {"giantsteps-tempo": giantsteps_excluded, "ballroom-beats": ballroom_excluded},
    }
    write_json(checked_path(root, "evaluation-plan.json"), plan)
    giantsteps_tracks = prepare_giantsteps(root, giantsteps, workers)
    ballroom_tracks, audio_inspection = prepare_ballroom(
        root, cache / "ballroom-data1.tar.gz", ballroom, replicas)
    common = {"annotation_format": "automix_annotation_v1",
              "normalization_revision": f"{NORMALIZATION_REVISION};sha256:{converter}"}
    corpora = [{
        **common,
        "id": "giantsteps-tempo", "dataset_name": "GiantSteps Tempo",
        "dataset_version": "v2 MIREX annotations (2018)",
        "source_revision": GIANTSTEPS_REVISION,
        "source_url": f"https://github.com/GiantSteps/giantsteps-tempo-dataset/tree/{GIANTSTEPS_REVISION}",
        "license_note": "Beatport previews from authors' mirror; no standalone repository license; local evaluation only; media excluded from Git and packages",
        "split_id": "giantsteps-v2-all-valid-661-evaluation-v1",
        "metrics": sorted(REQUIRED_METRICS["giantsteps-tempo"]),
        "expected_track_count": 664, "tracks": giantsteps_tracks, "exclusions": giantsteps_excluded,
    }, {
        **common,
        "id": "ballroom-beats", "dataset_name": "Ballroom",
        "dataset_version": "Krebs beat/bar annotations, ISMIR 2004 audio",
        "source_revision": BALLROOM_REVISION,
        "source_url": f"https://github.com/CPJKU/BallroomAnnotations/tree/{BALLROOM_REVISION}",
        "license_note": "mirdata reports CC BY-NC-SA 4.0; local noncommercial evaluation only; media excluded from Git and packages; cite Krebs et al. 2013",
        "split_id": "ballroom-author-deduplicated-685-evaluation-v1",
        "metrics": sorted(REQUIRED_METRICS["ballroom-beats"]),
        "expected_track_count": 698, "tracks": ballroom_tracks, "exclusions": ballroom_excluded,
    }]
    manifest = {"schema_version": 1, "corpora": sorted(corpora, key=lambda corpus: corpus["id"])}
    manifest_path = checked_path(root, "manifest.json")
    write_json(manifest_path, manifest)
    write_json(checked_path(root, "provenance.json"), {
        "schema_version": 1, "evaluation_plan_sha256": file_digest(root / "evaluation-plan.json"),
        "manifest_sha256": file_digest(manifest_path), "sources": SOURCES,
        "archive_inspections": {"giantsteps-tempo.zip": giantsteps_inspection,
                                "ballroom-annotations.zip": ballroom_inspection,
                                "ballroom-data1.tar.gz": audio_inspection},
    })
    print(f"Pinned corpus ready: {manifest_path}\nSHA-256: {file_digest(manifest_path)}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-cache", type=Path, default=Path("target/automix-corpus-sources"))
    parser.add_argument("--out", type=Path, default=Path("target/automix-corpus-data"))
    parser.add_argument("--workers", type=int, choices=range(1, 9), default=4)
    args = parser.parse_args()
    try:
        prepare(args.source_cache, args.out, args.workers)
    except (OSError, ValueError, KeyError, http.client.HTTPException,
            tarfile.TarError, zipfile.BadZipFile) as error:
        parser.exit(1, f"Public corpus preparation failed: {error}\n")


if __name__ == "__main__":
    main()
