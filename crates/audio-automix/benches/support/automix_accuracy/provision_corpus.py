"""Provision pinned AutoMix evaluation inputs before invoking the offline bench."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import tempfile
from urllib.parse import quote, urlsplit
from urllib.request import urlopen


REQUIRED_METRICS = {
    "giantsteps-tempo": {"tempo_accuracy1", "tempo_accuracy2"},
    "ballroom-beats": {"beat_f_measure", "beat_amlt"},
}


def checked_digest(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
        raise ValueError("expected a lowercase SHA-256 digest")
    return value


def checked_url(url):
    parsed = urlsplit(url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.fragment:
        raise ValueError("corpus sources must be HTTPS URLs without credentials or fragments")
    return url


def checked_path(root, name):
    if not isinstance(name, str) or "\\" in name or ":" in name:
        raise ValueError("invalid corpus path")
    if any(part in {"", ".", ".."} for part in name.split("/")):
        raise ValueError("corpus paths must be relative without traversal")
    path = root.joinpath(*name.split("/"))
    if not path.resolve().is_relative_to(root.resolve()):
        raise ValueError("corpus path escapes destination")
    return path


def download(url, destination, expected, *, algorithm="sha256", max_bytes=None):
    if algorithm == "sha256":
        checked_digest(expected)
    elif algorithm != "md5" or not re.fullmatch(r"[0-9a-f]{32}", expected):
        raise ValueError("expected a pinned SHA-256 or upstream MD5 digest")
    checked_url(url)
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with urlopen(url, timeout=60) as response:
            checked_url(response.geturl())
            with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
                temporary = Path(output.name)
                digest = hashlib.new(algorithm)
                size = 0
                while chunk := response.read(1024 * 1024):
                    size += len(chunk)
                    if max_bytes is not None and size > max_bytes:
                        raise ValueError(f"download exceeds size limit: {destination.name}")
                    output.write(chunk)
                    digest.update(chunk)
        if digest.hexdigest() != expected:
            label = "SHA-256" if algorithm == "sha256" else "MD5"
            raise ValueError(f"{label} mismatch: {destination.name}")
        temporary.replace(destination)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()


def provision(manifest_url, manifest_sha256, data_url, root):
    checked_url(data_url)
    if urlsplit(data_url).query:
        raise ValueError("data URL must identify a directory without a query")
    root = Path(root).resolve()
    root.mkdir(parents=True, exist_ok=True)
    manifest_path = root / "manifest.json"
    download(manifest_url, manifest_path, manifest_sha256)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("schema_version") != 1:
        raise ValueError("unsupported corpus manifest version")
    corpora = manifest["corpora"]
    by_id = {corpus["id"]: corpus for corpus in corpora}
    if len(by_id) != len(corpora):
        raise ValueError("duplicate corpus IDs")
    for corpus_id, required in REQUIRED_METRICS.items():
        if corpus_id not in by_id or not required.issubset(by_id[corpus_id]["metrics"]):
            raise ValueError(f"missing required evaluation metrics: {corpus_id}")

    # Validate every destination before fetching any media. The Rust runner
    # additionally validates labels, coverage, exclusions and split leakage.
    artifacts = {}
    for corpus in corpora:
        if not corpus["tracks"]:
            raise ValueError(f"empty corpus: {corpus['id']}")
        for track in corpus["tracks"]:
            for kind in ("audio", "annotation"):
                artifact = track[kind]
                name = artifact["path"]
                path = checked_path(root, name)
                if path == manifest_path:
                    raise ValueError("an artifact cannot replace the manifest")
                digest = checked_digest(artifact["sha256"])
                if name in artifacts and artifacts[name][1] != digest:
                    raise ValueError(f"conflicting artifact digests: {name}")
                artifacts[name] = (path, digest)
    for name, (path, digest) in sorted(artifacts.items()):
        download(data_url.rstrip("/") + "/" + quote(name, safe="/"), path, digest)
    return manifest_path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest-url", required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--data-url", required=True)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    try:
        path = provision(args.manifest_url, args.manifest_sha256, args.data_url, args.out)
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f"AutoMix corpus provisioning failed: {error}\n")
    print(f"Pinned corpus ready: {path}")


if __name__ == "__main__":
    main()
