#!/usr/bin/env python3
import gzip
import hashlib
import json
from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parent.parent
VENDOR = ROOT / "vendor"


def load_json(path):
    return json.loads(path.read_text())


def verify_cargo():
    lock = tomllib.loads((ROOT / "oracle/Cargo.lock").read_text())
    packages = {(p["name"], p["version"]): p for p in lock["package"] if "source" in p}
    rows = load_json(VENDOR / "cargo-manifest.json")
    directories = {p.name for p in (VENDOR / "cargo").iterdir() if p.is_dir()}
    if directories != {row["directory"] for row in rows} or len(rows) != len(packages):
        raise ValueError("vendored Cargo package count or directories differ")
    seen = set()
    checked = 0
    for row in rows:
        crate = VENDOR / "cargo" / row["directory"]
        meta = tomllib.loads((crate / "Cargo.toml").read_text())["package"]
        key = (meta["name"], meta["version"])
        if key in seen:
            raise ValueError(f"duplicate vendored crate {key}")
        seen.add(key)
        package = packages[key]
        checksums = load_json(crate / ".cargo-checksum.json")
        if (row["name"], row["version"]) != key or row["source"] != package["source"]:
            raise ValueError(f"Cargo manifest differs from lock: {key}")
        if checksums["package"] != package.get("checksum") or row["package_checksum"] != checksums["package"]:
            raise ValueError(f"Cargo package checksum differs from lock: {key}")
        if row["license"] != meta.get("license") or row["license_file"] != meta.get("license-file"):
            raise ValueError(f"Cargo license declaration differs: {key}")
        for name, expected in checksums["files"].items():
            actual = hashlib.sha256((crate / name).read_bytes()).hexdigest()
            if actual != expected:
                raise ValueError(f"vendored Cargo file checksum mismatch: {crate.name}/{name}")
            checked += 1
    if seen != packages.keys():
        raise ValueError("Cargo vendor set differs from lock")
    print(f"offline vendor: {len(rows)} Cargo packages, {checked} file checksums verified")


def verify_djs_inputs():
    lock = tomllib.loads((ROOT / "oracle/Cargo.lock").read_text())
    source = next(p["source"] for p in lock["package"] if p["name"] == "djs-core")
    manifest = load_json(VENDOR / "djs-inputs.json")
    if manifest["source"] != source or manifest["license"] != "MIT OR Apache-2.0":
        raise ValueError("DJS auxiliary input provenance differs from lock")
    expected = {
        "builtins/gen/prelude.js": "builtins/gen/prelude.js",
        "builtins/gen/prelude-slim.js": "builtins/gen/prelude-slim.js",
        "builtins/gen/prelude-noembed.js": "builtins/gen/prelude-noembed.js",
        "licenses/djs/LICENSE-APACHE": "LICENSE-APACHE",
        "licenses/djs/LICENSE-MIT": "LICENSE-MIT",
    }
    rows = manifest["files"]
    if len(rows) != len(expected) or {row["path"] for row in rows} != expected.keys():
        raise ValueError("DJS auxiliary input set differs")
    for row in rows:
        if row["upstream_path"] != "crates/patches/djs/" + expected[row["path"]]:
            raise ValueError(f"DJS upstream path differs: {row['path']}")
        data = (VENDOR / row["path"]).read_bytes()
        if len(data) != row["bytes"] or hashlib.sha256(data).hexdigest() != row["sha256"]:
            raise ValueError(f"DJS auxiliary input checksum mismatch: {row['path']}")
        blob = hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()
        if blob != row["git_blob"]:
            raise ValueError(f"DJS auxiliary input Git blob differs: {row['path']}")
    print(f"offline vendor: {len(rows)} pinned DJS auxiliary inputs verified")


def verify_specs():
    rows = load_json(VENDOR / "specs/manifest.json")
    if sorted(row["name"] for row in rows) != ["cloudflare", "vercel"]:
        raise ValueError("API spec manifest differs")
    for row in rows:
        packed = (VENDOR / "specs" / (row["name"] + ".json.gz")).read_bytes()
        data = gzip.decompress(packed)
        if len(packed) != row["gzip_bytes"] or hashlib.sha256(packed).hexdigest() != row["gzip_sha256"]:
            raise ValueError(f"compressed spec checksum mismatch: {row['name']}")
        if len(data) != row["bytes"] or hashlib.sha256(data).hexdigest() != row["sha256"]:
            raise ValueError(f"spec checksum mismatch: {row['name']}")
    print(f"offline vendor: {len(rows)} pinned API specifications verified")


def main():
    verify_cargo()
    verify_specs()
    verify_djs_inputs()
    files = [p for p in VENDOR.rglob("*") if p.is_file()]
    total = sum(p.stat().st_size for p in files)
    largest = max(p.stat().st_size for p in files)
    print(f"offline vendor: {len(files)} files, {total} bytes, largest file {largest} bytes")


if __name__ == "__main__":
    main()
