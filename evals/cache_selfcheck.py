import concurrent.futures
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from unittest import mock

import cache_store as cache
import cache_r


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


def r_limits(path, ca, limits):
    original = path.read_bytes()
    record = cache_r.inspect_plan(path, ca)
    enforced = limits == "container"
    assert record["warm_limits"] == limits
    assert record["warm_limits_enforced"] is enforced
    assert record["warm_cpus"] == (1 if enforced else None)
    assert record["warm_memory_mb"] == (2048 if enforced else None)
    assert cache_r.limits_args(record) == (["--cpus", "1", "--memory", "2048m"] if enforced else [])
    host = {"NanoCpus": 1000000000 if enforced else 0, "Memory": 2147483648 if enforced else 0}
    cache_r.inspect_limits({"HostConfig": host}, record)
    for field in host:
        for value in (None, False, str(host[field]), host[field] + 1):
            rejects(lambda: cache_r.inspect_limits({"HostConfig": host | {field: value}}, record))
        rejects(lambda: cache_r.inspect_limits({"HostConfig": {key: value for key, value in host.items() if key != field}}, record))
    mutations = (("warm_limits", "none" if enforced else "container"), ("warm_limits", "task"),
        ("schema", "r-cache-plan-v1"), ("warm_limits", None), ("warm_limits_enforced", not enforced), ("warm_limits_enforced", int(enforced)),
        ("warm_cpus", 2), ("warm_cpus", True), ("warm_memory_mb", 4096), ("warm_memory_mb", "2048"))
    for field, value in mutations:
        path.write_bytes(cache.canonical(record | {field: value}))
        rejects(lambda: cache_r.inspect_plan(path, ca))
        rejects(lambda: cache_r.warm(path, None, 1, ca))
    for field in ("warm_limits", "warm_limits_enforced", "warm_cpus", "warm_memory_mb"):
        path.write_bytes(cache.canonical({key: value for key, value in record.items() if key != field}))
        rejects(lambda: cache_r.inspect_plan(path, ca))
        rejects(lambda: cache_r.warm(path, None, 1, ca))
    assert not path.with_name("warm-started.json").exists()
    path.write_bytes(original)
    default = cache_r.record_for(Path(record["task"]), record["download_timeout_sec"], ca)
    assert cache_r.limits_args(default) == ["--cpus", "1", "--memory", "2048m"]
    rejects(lambda: cache_r.plan(Path(record["task"]), path.parent / "bad-limits", 900, ca, "task"))
    assert not (path.parent / "bad-limits").exists()


def r_profile(root):
    task = root / "task"
    (task / "tests").mkdir(parents=True)
    (task / "task.toml").write_text('[environment]\ndocker_image="' + cache_r.IMAGE_REF + '"\n')
    (task / "instruction.md").write_text("mock task, not benchmark instructions\n")
    verifier = task / "tests" / "test.sh"
    verifier.write_text("mock verifier; never execute\n")
    assertions = task / "tests" / "test_outputs.py"
    assertions.write_text("mock assertions; never execute\n")
    pins = dict(TASK_DIGEST=cache_r.run.task_digest(task), VERIFIER_SHA256=cache.hash_file(verifier), ASSERTIONS_SHA256=cache.hash_file(assertions))
    ca = root / "ca.pem"
    ca.write_bytes(b"mock CA, not a real trust anchor")
    with mock.patch.multiple(cache_r, **pins), mock.patch.object(cache_r.wal, "docker", side_effect=AssertionError("offline plan invoked Docker")):
        paths = []
        for limits in ("container", "none"):
            profile_paths = []
            for number in range(3):
                result = cache_r.plan(task, root / limits / str(number), 900, ca, limits)
                profile_paths.append(Path(result["plan"]))
                assert result["network_started"] is False
                cache_r.inspect_plan(profile_paths[-1], ca)
            assert len({path.read_bytes() for path in profile_paths}) == 1
            assert len({path.with_name("setup.sh").read_bytes() for path in profile_paths}) == 1
            r_limits(profile_paths[0], ca, limits)
            paths.extend(profile_paths)
        assert paths[0].read_bytes() != paths[-1].read_bytes()
        assert paths[0].with_name("setup.sh").read_bytes() == paths[-1].with_name("setup.sh").read_bytes()
        path = paths[0]
        original = path.read_bytes()
        record = json.loads(original)
        for field, value in (("schema", "wal-cache-plan-v1"), ("task_digest", "0" * 64), ("verifier_sha256", "0" * 64), ("all_arms_required", False), ("assertions_sha256", "0" * 64), ("base_image_id", "sha256:" + "0" * 64), ("dependency_requests", {}), ("setup_transport", {}), ("setup_toolchain", {}), ("download_timeout_sec", 0)):
            path.write_bytes(cache.canonical(record | {field: value}))
            rejects(lambda: cache_r.inspect_plan(path, ca))
        path.write_bytes(original)
        setup = path.with_name("setup.sh")
        before = setup.read_bytes()
        setup.write_bytes(before + b"exit 0\n")
        rejects(lambda: cache_r.inspect_plan(path, ca))
        setup.write_bytes(before)
        for changed in (verifier, assertions, task / "instruction.md", ca):
            before = changed.read_bytes()
            changed.write_bytes(before + b"mutation")
            rejects(lambda: cache_r.inspect_plan(path, ca))
            changed.write_bytes(before)
        with mock.patch.dict(os.environ, {"HTTPS_PROXY": "http://changed.invalid:1"}):
            rejects(lambda: cache_r.inspect_plan(path, ca))
        rejects(lambda: cache_r.plan(task, root / "bad-timeout", 0, ca))
        script = setup.read_text()
        assert "--no-install-recommends" in script
        assert "r-base-core" in script
        assert 'cd "$capture"' in script
        assert "stage r-version Rscript --version\n" in script
        assert "Rscript --vanilla --version" not in script
        assert "Rscript --vanilla -e" in script
        assert script.index("stage r-version Rscript --version\n") < script.index("stage r-stats Rscript --vanilla -e") < script.index('task_tree > "$capture/task-after"')
        for name in ("pytest", "numpy", "scipy", "pytest-json-ctrf"):
            assert name + "==" + cache_r.DEPENDENCIES[name] in script
        assert "pytest --version" in script
        assert "/tests/test.sh" not in script
        assert "source(\"ars.R\")" not in script
        assert cache_r.inspect_plan(path, ca) == record
    return {"deterministic_plans_per_profile": 3, "profiles": ["container", "none"], "fixture_only": True, "task_assertion_profile_transport_setup_mutations_rejected": True, "docker_calls": 0}


def main():
    output = Path(__file__).resolve().parents[1] / "output"
    output.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="cache-selfcheck-", dir=output))
    checks = {
        "restart_mirror": restart_mirror(root / "mirror"),
        "atomic_race": atomic_race(root / "race"),
        "manifest": manifests(root / "manifest"),
        "r_profile": r_profile(root / "r-profile"),
    }
    result = {"passed": len(checks), "failed": 0, "checks": checks, "evidence_root": str(root), "real_network_downloads": 0, "docker_runs": 0, "model_requests": 0}
    (root / "summary.json").write_bytes(cache.canonical(result))
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
