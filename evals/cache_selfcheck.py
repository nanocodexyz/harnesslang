import concurrent.futures
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request

import cache_store as cache


def rejects(action):
    try:
        action()
    except (cache.CacheError, KeyError, ValueError):
        return
    raise AssertionError("invalid cache state accepted")


def artifact(store, value):
    return store.put(io.BytesIO(value), hashlib.sha256(value).hexdigest(), len(value))


def restart_mirror(root):
    store = cache.Store(root)
    payload = b"offline dependency seed\n"
    record = artifact(store, payload)
    program = """import sys,urllib.request,json
import cache_store as c
s=c.Store(sys.argv[1]); h=sys.argv[2]
with c.mirror(s) as (host,port):
 u='http://%s:%s/sha256/%s'%(host,port,h)
 value=urllib.request.urlopen(u,timeout=2).read()
 print(json.dumps({'bytes':len(value),'sha256':c.hashlib.sha256(value).hexdigest()}))
"""
    attempts = []
    for number in range(3):
        process = subprocess.run([sys.executable, "-c", program, str(root), record["sha256"]], cwd=Path(__file__).parent, capture_output=True, text=True, check=True, timeout=10)
        result = json.loads(process.stdout)
        assert result == record
        attempts.append({"attempt": number + 1, "fresh_process": True, **result})
    path = store.path(record["sha256"])
    path.chmod(0o644)
    path.write_bytes(b"corrupted seed")
    rejects(lambda: store.verified(record["sha256"]))
    with cache.mirror(store) as (host, port):
        try:
            urllib.request.urlopen(f"http://{host}:{port}/sha256/{record['sha256']}", timeout=2)
        except urllib.error.HTTPError as error:
            assert error.code == 404
        else:
            raise AssertionError("mirror served corrupt bytes")
    return {"fresh_process_attempts": attempts, "corrupt_read_rejected": True, "corrupt_mirror_rejected": True}


def atomic_race(root):
    store = cache.Store(root)
    payload = b"concurrent immutable artifact\n"
    sha256 = hashlib.sha256(payload).hexdigest()
    partial = store.blobs / ".partial-interrupted"
    partial.write_bytes(b"incomplete")
    rejects(lambda: store.verified(sha256))
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        records = list(pool.map(lambda _: artifact(store, payload), range(3)))
    assert len({record["sha256"] for record in records}) == 1
    assert store.verified(sha256).read_bytes() == payload
    rejects(lambda: store.put(io.BytesIO(payload), "0" * 64, len(payload)))
    rejects(lambda: store.put(io.BytesIO(payload), sha256, len(payload) - 1))
    assert store.verified(sha256).read_bytes() == payload
    rejects(lambda: store.fetch("https://example.com/seed", sha256, len(payload) - 1, 1))
    assert list(store.blobs.glob(".partial-*")) == [partial]
    rejects(lambda: store.path("../escape"))
    rejects(lambda: cache.Store("/tmp/not-durable"))
    for url in ("http://example.com/a", "https://user:secret@example.com/a", "https://example.com/a?token=secret"):
        rejects(lambda: cache.source_url(url))
    return {"writers": len(records), "partial_not_ready": True, "hash_bound_and_path_mutations_rejected": True}


def manifests(root):
    store = cache.Store(root)
    archive = artifact(store, b"mock docker image archive; not an actual image")
    source = artifact(store, b"mock installer")
    manifest = {
        "schema": cache.SCHEMA,
        "transport": "prewarmed-image+artifact-seed",
        "task_digest": "1" * 64,
        "verifier_sha256": "2" * 64,
        "setup_sha256": "3" * 64,
        "base_image_id": "sha256:" + "4" * 64,
        "warm_image_id": "sha256:" + "5" * 64,
        "image_archive": archive,
        "source_pins": [{"url": "https://astral.sh/uv/0.9.5/install.sh", **source}],
        "all_arms_required": True,
        "verifier_unchanged": True,
        "legacy_comparable": False,
        "network_required": True,
    }
    variant = cache.publish_variant(store, manifest)
    assert cache.load_variant(cache.Store(root), variant, {"task_digest": "1" * 64}) == manifest
    assert cache.publish_variant(store, manifest) == variant
    rejects(lambda: cache.load_variant(store, variant, {"task_digest": "0" * 64}))
    for field, value in (("legacy_comparable", True), ("network_required", False), ("verifier_unchanged", False), ("all_arms_required", False)):
        rejects(lambda: cache.publish_variant(store, manifest | {field: value}))
    assert cache.load_variant(store, variant) == manifest
    return {"variant_id": variant, "same_bytes_same_variant": True, "pin_and_comparison_mutations_rejected": True, "real_docker_image_tested": False}


def main():
    output = Path(__file__).resolve().parents[1] / "output"
    output.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="cache-selfcheck-", dir=output))
    checks = {
        "restart_mirror": restart_mirror(root / "mirror"),
        "atomic_race": atomic_race(root / "race"),
        "manifest": manifests(root / "manifest"),
    }
    result = {"passed": len(checks), "failed": 0, "checks": checks, "evidence_root": str(root), "real_network_downloads": 0, "docker_runs": 0, "model_requests": 0}
    (root / "summary.json").write_bytes(cache.canonical(result))
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
