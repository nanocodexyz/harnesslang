import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

import cache_store as cache
import cache_wal as wal
import run

BASE_IMAGE_ID = "sha256:985bed39e8448bc95f138b5f766b59003a95c4be171c96af8e76448338f3dd8f"
IMAGE_REF = "alexgshaw/adaptive-rejection-sampler:20251031"
TASK_DIGEST = "bcaa2399985cd57666018025846289ab25e193ae0dd8fb7f0ffab2410c24d4de"
VERIFIER_SHA256 = "38b43560d173cc2b952c3a3e17b8a480216d84e33450515e047bcb0d806b1e0a"
ASSERTIONS_SHA256 = "547dc6e107f034f41703722aeceb6d0236e3fb69116fc3f2fbaba11884de352f"
INSTALLER_SHA256 = "8402ab80d2ef54d7044a71ea4e4e1e8db3b20c87c7bffbc30bff59f1e80ebbd5"
CAPTURE = "/opt/hl-r-cache"
DEPENDENCIES = {"apt": ["ca-certificates", "curl", "r-base-core"], "install_recommends": False,
    "uv": "0.9.5", "python": "3.13", "pytest": "8.4.1", "numpy": "2.3.3",
    "scipy": "1.16.2", "pytest-json-ctrf": "0.3.5"}
FLAGS = {"network_required": True, "verifier_unchanged": True, "legacy_comparable": False, "all_arms_required": True}


def positive(value):
    if type(value) is not int or value <= 0:
        raise cache.CacheError("timeout must be a positive integer")
    return value


def transport(ca_file):
    return {"proxy_environment_sha256": {key: hashlib.sha256(cache.canonical(os.environ.get(key))).hexdigest() for key in wal.PROXY_ENV},
        "ca_sha256": cache.hash_file(ca_file) if ca_file else None, "network": "host"}


def setup_script(seconds):
    positive(seconds)
    requirements = " ".join("-w " + name + "==" + DEPENDENCIES[name] for name in ("pytest", "numpy", "scipy", "pytest-json-ctrf"))
    return f'''set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
capture={CAPTURE}
mkdir -p "$capture/debs/partial"
stage() {{ printf '%s\\n' "stage=$1 timeout_sec={seconds}"; shift; timeout {seconds} "$@"; }}
apt_stage() {{ stage "$1" apt-get -o Acquire::Retries=0 -o Acquire::http::Timeout=15 -o Acquire::https::Timeout=15 -o APT::Update::Error-Mode=any -o Dir::Cache::archives="$capture/debs" "${{@:2}}"; }}
task_tree() {{ find /app /protected -printf '%p %y %m %l\\0' | sort -z; find /app /protected -type f -print0 | sort -z | xargs -0 -r sha256sum; }}
task_tree > "$capture/task-before"
cd "$capture"
apt_stage apt-index update
apt_stage apt-resolution --print-uris --download-only --no-install-recommends install {' '.join(DEPENDENCIES['apt'])} > "$capture/apt-uris.txt"
apt_stage apt-download --download-only --no-install-recommends -y install {' '.join(DEPENDENCIES['apt'])}
apt_stage apt-install --no-download --no-install-recommends -y install {' '.join(DEPENDENCIES['apt'])}
cp -a /etc/apt "$capture/apt-config"
cp -a /var/lib/apt/lists "$capture/apt-lists"
dpkg-query -W -f='${{binary:Package}}\\t${{Version}}\\t${{Architecture}}\\n' > "$capture/packages.tsv"
find "$capture/debs" -type f -name '*.deb' -print0 | sort -z | xargs -0 -r sha256sum > "$capture/debs.sha256"
stage uv-installer curl --proto '=https' --proto-redir '=https' --max-time {seconds} -LsSf {wal.INSTALLER_URL} -o "$capture/uv-install.sh"
printf '%s  %s\\n' {INSTALLER_SHA256} "$capture/uv-install.sh" | sha256sum -c -
stage uv-install sh "$capture/uv-install.sh"
. "$HOME/.local/bin/env"
sha256sum "$(command -v uv)" > "$capture/uv-binary.sha256"
uv --version > "$capture/uv-version.txt"
test "$(cat "$capture/uv-version.txt")" = 'uv {DEPENDENCIES['uv']}'
stage python-scientific-cache uvx -p {DEPENDENCIES['python']} {requirements} pytest --version
uv python list --only-installed --output-format json > "$capture/python.json"
find "$(uv python dir)" -type f -print0 | sort -z | xargs -0 -r sha256sum > "$capture/python-files.sha256"
cache_dir=$(uv cache dir)
find "$cache_dir" -type f -print0 | sort -z | xargs -0 -r sha256sum > "$capture/uv-cache.sha256"
find "$cache_dir" -name METADATA -type f -print0 | sort -z | xargs -0 -r grep -HE '^(Name|Version):' > "$capture/python-packages.txt"
stage r-version Rscript --version
stage r-stats Rscript --vanilla -e 'stopifnot(requireNamespace("stats", quietly=TRUE))'
task_tree > "$capture/task-after"
cmp "$capture/task-before" "$capture/task-after"
printf '%s\\n' 'stage=dependency-setup-complete'
'''.encode()


def limits_profile(limits):
    if limits not in ("container", "none"):
        raise cache.CacheError("R limits must be container or none")
    enforced = limits == "container"
    return {"warm_limits": limits, "warm_limits_enforced": enforced,
        "warm_cpus": 1 if enforced else None, "warm_memory_mb": 2048 if enforced else None}


def limits_args(record):
    expected = limits_profile(record["warm_limits"])
    if cache.canonical({key: record[key] for key in expected}) != cache.canonical(expected):
        raise cache.CacheError("R resource profile changed")
    return ["--cpus", str(record["warm_cpus"]), "--memory", str(record["warm_memory_mb"]) + "m"] if record["warm_limits_enforced"] else []


def inspect_limits(info, record):
    limits_args(record)
    expected = {"NanoCpus": record["warm_cpus"] * 1000000000, "Memory": record["warm_memory_mb"] * 1024 * 1024} if record["warm_limits_enforced"] else {"NanoCpus": 0, "Memory": 0}
    actual = info.get("HostConfig", {})
    if any(type(actual.get(key)) is not int or actual[key] != value for key, value in expected.items()):
        raise cache.CacheError("Docker R resource settings differ from pinned profile")


def record_for(task, seconds, ca_file, limits="container"):
    profile = limits_profile(limits)
    task = task.resolve()
    if run.task_digest(task) != TASK_DIGEST or run.task_config(task)["image"] != IMAGE_REF:
        raise cache.CacheError("R task differs from inspected package")
    for relative, expected in (("tests/test.sh", VERIFIER_SHA256), ("tests/test_outputs.py", ASSERTIONS_SHA256)):
        if cache.hash_file(task / relative) != expected:
            raise cache.CacheError("R verifier or assertions changed")
    return {"schema": "r-cache-plan-v2", "task": str(task), "task_digest": TASK_DIGEST,
        "verifier_sha256": VERIFIER_SHA256, "assertions_sha256": ASSERTIONS_SHA256,
        "base_image_id": BASE_IMAGE_ID, "setup_sha256": hashlib.sha256(setup_script(seconds)).hexdigest(),
        "download_timeout_sec": seconds, "dependency_requests": DEPENDENCIES, "setup_transport": transport(ca_file),
        "setup_toolchain": {name: cache.hash_file(Path(__file__).with_name(name)) for name in ("cache_r.py", "cache_wal.py", "cache_store.py", "run.py")},
        **profile,
        "scope": "dependency setup only; no task verifier, solution, reward or model", **FLAGS}


def plan(task, output, seconds, ca_file=None, limits="container"):
    record = record_for(task, seconds, ca_file, limits)
    output = output.resolve()
    if not output.is_relative_to(Path.home().resolve()):
        raise cache.CacheError("plan must be restart-durable under home")
    output.mkdir(parents=True, exist_ok=False)
    wal.exclusive_write(output / "setup.sh", setup_script(seconds))
    wal.exclusive_write(output / "plan.json", cache.canonical(record))
    return {"plan": str(output / "plan.json"), "network_started": False, "setup_sha256": record["setup_sha256"]}


def inspect_plan(path, ca_file=None):
    record = json.loads(path.read_bytes())
    if not isinstance(record, dict) or record.get("schema") != "r-cache-plan-v2":
        raise cache.CacheError("unsupported R plan; create a new profile-pinned plan")
    expected = record_for(Path(record["task"]), record["download_timeout_sec"], ca_file, record["warm_limits"])
    if cache.canonical(record) != cache.canonical(expected) or cache.hash_file(path.parent / "setup.sh") != record["setup_sha256"]:
        raise cache.CacheError("R plan, setup, profile, transport or toolchain changed")
    return record


def captured_manifest(record, capture, image, archive, source):
    if (capture / "task-before").read_bytes() != (capture / "task-after").read_bytes():
        raise cache.CacheError("task contents changed during dependency setup")
    if cache.hash_file(capture / "uv-install.sh") != INSTALLER_SHA256:
        raise cache.CacheError("unexpected uv installer")
    if (capture / "uv-version.txt").read_text().strip() != "uv " + DEPENDENCIES["uv"]:
        raise cache.CacheError("unexpected installed uv version")
    return {**record, "transport": "prewarmed-image+artifact-seed",
        "schema": cache.SCHEMA, "warm_image_id": image, "image_archive": archive,
        "source_pins": [{"url": wal.INSTALLER_URL, **source}],
        "installed_uv": (capture / "uv-version.txt").read_text().strip(),
        "installed_python": json.loads((capture / "python.json").read_bytes()),
        "capture_files": {str(p.relative_to(capture)): {"sha256": cache.hash_file(p), "bytes": p.stat().st_size} for p in sorted(capture.rglob("*")) if p.is_file()},
        "remaining_network": ["unchanged verifier apt indexes/curl updates", "unconditional uv HTTPS installer and release resolution"]}


def warm(plan_path, store, timeout_sec, ca_file=None):
    positive(timeout_sec)
    plan_path = plan_path.resolve()
    record = inspect_plan(plan_path, ca_file)
    deadline = time.monotonic() + timeout_sec
    def docker(args, **kwargs):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise cache.CacheError("total warm timeout exhausted")
        return wal.docker(args, remaining, **kwargs)
    output = plan_path.parent
    name = "hl-r-cache-" + cache.hash_file(plan_path)[:16]
    wal.exclusive_write(output / "warm-started.json", cache.canonical({"plan_sha256": cache.hash_file(plan_path), "container": name}))
    info = json.loads(docker(["image", "inspect", BASE_IMAGE_ID], capture_output=True, text=True).stdout)[0]
    if info["Id"] != BASE_IMAGE_ID or info["Architecture"] != "amd64" or info["Os"] != "linux" or info["Config"].get("WorkingDir") != "/app":
        raise cache.CacheError("unexpected R base image/platform/workdir")
    if info["Config"].get("Entrypoint") or not info["Config"].get("Cmd"):
        raise cache.CacheError("R base must have no entrypoint and a restorable command")
    docker(["create", *limits_args(record), "--network", "host", "--name", name, BASE_IMAGE_ID, "sleep", str(timeout_sec)], stdout=subprocess.DEVNULL)
    docker(["start", name], stdout=subprocess.DEVNULL)
    inspect_limits(json.loads(docker(["inspect", name], capture_output=True, text=True).stdout)[0], record)
    docker(["cp", str(output / "setup.sh"), name + ":/tmp/hl-r-setup.sh"])
    env = [part for key in wal.PROXY_ENV for part in ("-e", key if key in os.environ else key + "=")]
    env += ["-e", "UV_CONCURRENT_DOWNLOADS=2", "-e", "UV_CONCURRENT_BUILDS=2", "-e", "UV_CONCURRENT_INSTALLS=2"]
    if ca_file:
        docker(["cp", str(ca_file), name + ":/tmp/hl-r-ca.crt"])
        env += ["-e", "SSL_CERT_FILE=/tmp/hl-r-ca.crt", "-e", "CURL_CA_BUNDLE=/tmp/hl-r-ca.crt"]
    with (output / "warm.log").open("xb") as log:
        try:
            docker(["exec", "-u", "0", *env, name, "bash", "/tmp/hl-r-setup.sh"], stdout=log, stderr=subprocess.STDOUT)
        finally:
            log.flush()
            os.fsync(log.fileno())
    inspect_plan(plan_path, ca_file)
    docker(["exec", name, "rm", "-f", "/tmp/hl-r-setup.sh", "/tmp/hl-r-ca.crt"])
    capture = output / "capture"
    docker(["cp", name + ":" + CAPTURE, str(capture)])
    source = store.put_file(capture / "uv-install.sh", INSTALLER_SHA256, (capture / "uv-install.sh").stat().st_size)
    image = docker(["commit", "--change", "CMD " + json.dumps(info["Config"]["Cmd"]), name], capture_output=True, text=True).stdout.strip()
    cache.digest(image.removeprefix("sha256:"))
    committed = json.loads(docker(["image", "inspect", image], capture_output=True, text=True).stdout)[0]
    if committed["Config"] != info["Config"]:
        raise cache.CacheError("dependency warm changed image configuration")
    archive_path = output / "image.tar"
    docker(["image", "save", "-o", str(archive_path), image])
    archive = store.put_file(archive_path, cache.hash_file(archive_path), archive_path.stat().st_size)
    manifest = captured_manifest(record, capture, image, archive, source)
    inspect_plan(plan_path, ca_file)
    docker(["rm", "-f", name], stdout=subprocess.DEVNULL)
    inspect_plan(plan_path, ca_file)
    variant = cache.publish_variant(store, manifest)
    wal.exclusive_write(output / "variant.json", cache.canonical({"variant_id": variant, "manifest": manifest}))
    return {"variant_id": variant, "warm_image_id": image, "network_required": True, "real_verifier_tested": False}


def main():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("plan")
    prepare.add_argument("--task", type=Path, required=True)
    prepare.add_argument("--output", type=Path, required=True)
    prepare.add_argument("--download-timeout-sec", type=int, required=True)
    prepare.add_argument("--ca-file", type=Path)
    prepare.add_argument("--limits", choices=("container", "none"), default="container")
    execute = commands.add_parser("warm")
    execute.add_argument("--plan", type=Path, required=True)
    execute.add_argument("--store", type=Path, default=cache.DEFAULT_ROOT)
    execute.add_argument("--timeout-sec", type=int, required=True)
    execute.add_argument("--ca-file", type=Path)
    execute.add_argument("--authorize-network-setup", action="store_true", required=True)
    args = parser.parse_args()
    result = plan(args.task, args.output, args.download_timeout_sec, args.ca_file, args.limits) if args.command == "plan" else warm(args.plan, cache.Store(args.store), args.timeout_sec, args.ca_file)
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
