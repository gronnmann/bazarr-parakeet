#!/usr/bin/env python3
"""Download a pinned TDT ONNX model, verify hashes, and install files atomically.

Python standard library only. Override repo/revision/subdir for compatible models.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile
import urllib.parse
import urllib.request

DEFAULT_REVISION = "4d2a8bc71f5c896ec40faa59732e6716295edaf2"
REQUIRED = {"encoder-model.onnx", "decoder_joint-model.onnx", "vocab.txt"}
OPTIONAL = {"encoder-model.onnx.data", "decoder_joint-model.onnx.data"}


def request(url):
    headers = {"User-Agent": "bazarr-parakeet/0.1"}
    if os.environ.get("HF_TOKEN"):
        headers["Authorization"] = "Bearer " + os.environ["HF_TOKEN"]
    return urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=120)


def digest(path, entry):
    lfs = entry.get("lfs")
    hasher = hashlib.sha256() if lfs else hashlib.sha1()
    if not lfs:
        hasher.update(f"blob {entry['size']}\0".encode())
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def valid(path, entry):
    expected = entry["lfs"]["oid"] if entry.get("lfs") else entry["oid"]
    return path.is_file() and path.stat().st_size == entry["size"] and digest(path, entry) == expected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default="altunenes/parakeet-rs")
    parser.add_argument("--revision", default=DEFAULT_REVISION)
    parser.add_argument("--subdir", default="parakeet-ultra")
    parser.add_argument("--output", type=Path, default=Path("models/ultra"))
    args = parser.parse_args()
    repo = urllib.parse.quote(args.repo, safe="/")
    revision = urllib.parse.quote(args.revision, safe="")
    with request(f"https://huggingface.co/api/models/{repo}/revision/{revision}") as response:
        commit = json.load(response)["sha"]
    subdir = args.subdir.strip("/")
    suffix = "/" + urllib.parse.quote(subdir, safe="/") if subdir else ""
    with request(f"https://huggingface.co/api/models/{repo}/tree/{commit}{suffix}?limit=1000") as response:
        entries = json.load(response)
    files = {Path(e["path"]).name: e for e in entries if e["type"] == "file"
             and Path(e["path"]).name in REQUIRED | OPTIONAL}
    missing = REQUIRED - files.keys()
    if missing:
        raise SystemExit(f"Model is missing required TDT files: {', '.join(sorted(missing))}")
    args.output.mkdir(parents=True, exist_ok=True)
    for name, entry in sorted(files.items()):
        destination = args.output / name
        if valid(destination, entry):
            print(f"Verified {name}", flush=True)
            continue
        print(f"Downloading {name} ({entry['size'] / 1024**2:.1f} MiB)", flush=True)
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(dir=args.output, prefix=".download-", delete=False) as file:
                temporary = Path(file.name)
                path = urllib.parse.quote(entry["path"], safe="/")
                with request(f"https://huggingface.co/{repo}/resolve/{commit}/{path}") as response:
                    for chunk in iter(lambda: response.read(1024 * 1024), b""):
                        file.write(chunk)
                file.flush()
                os.fsync(file.fileno())
            if not valid(temporary, entry):
                raise RuntimeError(f"Checksum or size mismatch for {name}")
            temporary.chmod(0o644)
            temporary.replace(destination)
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
    manifest = {"repo": args.repo, "revision": commit, "subdir": subdir,
                "upstream": "https://huggingface.co/moondream/parakeet-ultra" if subdir == "parakeet-ultra" else None,
                "files": files}
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Ready: {args.output} ({commit})", flush=True)


if __name__ == "__main__":
    main()
