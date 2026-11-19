import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

import cache_store as cache
import run

BASE_IMAGE_ID = "sha256:0e33ea5ec823975d1bd6c3778395c9f94251dd88f571146057bff6adb7e4594e"
VERIFIER_SHA256 = "4770437ea96c3cc84684b4f99d55fb148fcac09f9ea1e8ef49de487716e6c334"
INSTALLER_URL = "https://astral.sh/uv/0.9.5/install.sh"
PROXY_ENV = ("http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY", "no_proxy", "NO_PROXY")


def exclusive_write(path, value):
    with path.open("xb") as destination:
        destination.write(value)
        destination.flush()
        os.fsync(destination.fileno())


def docker(args, timeout_sec, **kwargs):
    return subprocess.run(["docker", *args], timeout=timeout_sec, check=True, **kwargs)


def setup_script(download_timeout_sec):
    return f'''set -eu
printf '%s\\n' 'stage=apt-index timeout_sec={download_timeout_sec}'
timeout {download_timeout_sec} apt-get -o Acquire::Retries=0 -o Acquire::http::Timeout=15 -o Acquire::https::Timeout=15 -o APT::Update::Error-Mode=any update
printf '%s\\n' 'stage=curl-install timeout_sec={download_timeout_sec}'
timeout {download_timeout_sec} apt-get -o Acquire::Retries=0 -o Acquire::http::Timeout=15 -o Acquire::https::Timeout=15 install -y curl
mkdir -p /cache-capture
printf '%s\\n' 'stage=uv-installer-fetch timeout_sec={download_timeout_sec}'
curl --max-time {download_timeout_sec} -LsSf {INSTALLER_URL} -o /cache-capture/uv-install.sh
printf '%s\\n' 'stage=uv-install timeout_sec={download_timeout_sec}'
timeout {download_timeout_sec} sh /cache-capture/uv-install.sh
. "$HOME/.local/bin/env"
printf '%s\\n' 'stage=python-pytest-cache timeout_sec={download_timeout_sec}'
timeout {download_timeout_sec} uvx -p 3.13 -w pytest==8.4.1 -w pytest-json-ctrf==0.3.5 pytest --version
uv --version > /cache-capture/uv-version.txt
uv python list --only-installed --output-format json > /cache-capture/python.json
uv cache dir > /cache-capture/cache-dir.txt
printf '%s\\n' 'stage=dependency-setup-complete'
'''.encode()


def plan(task, output, download_timeout_sec):
    task = task.resolve()
    if download_timeout_sec <= 0:
        raise cache.CacheError("download timeout must be positive")
    if cache.hash_file(task / "tests" / "test.sh") != VERIFIER_SHA256:
        raise cache.CacheError("WAL verifier differs from inspected public task")
    if run.task_config(task)["image"] != "alexgshaw/db-wal-recovery:20251031":
        raise cache.CacheError("unexpected WAL image reference")
    output = output.resolve()
    if not output.is_relative_to(Path.home().resolve()):
        raise cache.CacheError("plan output must be restart-durable under home")
    output.mkdir(parents=True, exist_ok=False)
    setup = setup_script(download_timeout_sec)
    exclusive_write(output / "setup.sh", setup)
    record = {
        "schema": "wal-cache-plan-v1",
        "task": str(task),
        "task_digest": run.task_digest(task),
        "verifier_sha256": VERIFIER_SHA256,
        "base_image_id": BASE_IMAGE_ID,
        "setup_sha256": hashlib.sha256(setup).hexdigest(),
        "installer_url": INSTALLER_URL,
        "dependency_requests": {"uv": "0.9.5", "python": "3.13", "pytest": "8.4.1", "pytest-json-ctrf": "0.3.5"},
        "network_required": True,
        "verifier_unchanged": True,
        "legacy_comparable": False,
        "all_arms_required": True,
        "scope": "dependency setup only; no tests, task execution, solution, reward, or model",
    }
    exclusive_write(output / "plan.json", cache.canonical(record))
    return {"plan": str(output / "plan.json"), "network_started": False, "setup_sha256": record["setup_sha256"]}


def inspect_plan(plan_path):
    record = json.loads(plan_path.read_bytes())
    if record["schema"] != "wal-cache-plan-v1" or record["base_image_id"] != BASE_IMAGE_ID:
        raise cache.CacheError("unsupported WAL plan")
    task = Path(record["task"])
    if run.task_digest(task) != record["task_digest"] or cache.hash_file(task / "tests" / "test.sh") != VERIFIER_SHA256:
        raise cache.CacheError("task or verifier changed since plan")
    setup = plan_path.parent / "setup.sh"
    if cache.hash_file(setup) != record["setup_sha256"]:
        raise cache.CacheError("setup script changed since plan")
    return record


def warm(plan_path, store, timeout_sec, ca_file):
    plan_path = plan_path.resolve()
    record = inspect_plan(plan_path)
    output = plan_path.parent
    lock = output / "warm-started.json"
    exclusive_write(lock, cache.canonical({"plan_sha256": cache.hash_file(plan_path)}))
    prefix = "hl-wal-cache-" + record["setup_sha256"][:16]
    docker(["image", "inspect", BASE_IMAGE_ID], 30, stdout=subprocess.DEVNULL)
    capture = output / "capture"
    capture.mkdir()
    args = ["create", "--network", "host", "--mount", "type=bind,source=" + str(capture) + ",target=/cache-capture", "--name", prefix, "--entrypoint", "", BASE_IMAGE_ID, "sleep", str(timeout_sec)]
    docker(args, 30, stdout=subprocess.DEVNULL)
    try:
        docker(["start", prefix], 30, stdout=subprocess.DEVNULL)
        docker(["cp", str(output / "setup.sh"), prefix + ":/tmp/cache-setup.sh"], 30)
        env = [part for key in PROXY_ENV if key in os.environ for part in ("-e", key)]
        env += ["-e", "UV_CONCURRENT_DOWNLOADS=2", "-e", "UV_CONCURRENT_BUILDS=2", "-e", "UV_CONCURRENT_INSTALLS=2"]
        if ca_file:
            docker(["cp", str(ca_file), prefix + ":/tmp/cache-ca.crt"], 30)
            env += ["-e", "SSL_CERT_FILE=/tmp/cache-ca.crt", "-e", "CURL_CA_BUNDLE=/tmp/cache-ca.crt"]
        docker(["exec", "-u", "0", *env, prefix, "bash", "/tmp/cache-setup.sh"], timeout_sec, stdout=sys.stdout, stderr=sys.stderr)
        docker(["exec", prefix, "rm", "-rf", "/tmp/cache-setup.sh", "/tmp/cache-ca.crt"], 30)
        image = docker(["commit", prefix], 60, capture_output=True, text=True).stdout.strip()
        cache.digest(image.removeprefix("sha256:"))
        archive_path = output / "image.tar"
        docker(["image", "save", "-o", str(archive_path), image], timeout_sec)
        archive = store.put_file(archive_path, cache.hash_file(archive_path), archive_path.stat().st_size)
        installer = capture / "uv-install.sh"
        source = store.put_file(installer, cache.hash_file(installer), installer.stat().st_size)
        manifest = {
            "schema": cache.SCHEMA,
            "transport": "prewarmed-image+artifact-seed",
            **{key: record[key] for key in ("task_digest", "verifier_sha256", "setup_sha256", "base_image_id", "network_required", "verifier_unchanged", "legacy_comparable", "all_arms_required")},
            "warm_image_id": image,
            "image_archive": archive,
            "source_pins": [{"url": INSTALLER_URL, **source}],
            "dependency_requests": record["dependency_requests"],
            "installed_python": json.loads((capture / "python.json").read_bytes()),
            "installed_uv": (capture / "uv-version.txt").read_text().strip(),
            "remaining_network": ["apt indexes", "unconditional HTTPS uv installer fetch and HTTPS release resolution"],
        }
        variant = cache.publish_variant(store, manifest)
        exclusive_write(output / "variant.json", cache.canonical({"variant_id": variant, "manifest": manifest}))
        if run.task_digest(Path(record["task"])) != record["task_digest"]:
            raise cache.CacheError("task changed during warm-up")
        return {"variant_id": variant, "warm_image_id": image, "network_required": True, "real_verifier_tested": False}
    finally:
        docker(["rm", "-f", prefix], 30, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def main():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("plan")
    prepare.add_argument("--task", type=Path, required=True)
    prepare.add_argument("--output", type=Path, required=True)
    prepare.add_argument("--download-timeout-sec", type=int, required=True)
    execute = commands.add_parser("warm")
    execute.add_argument("--plan", type=Path, required=True)
    execute.add_argument("--store", type=Path, default=cache.DEFAULT_ROOT)
    execute.add_argument("--timeout-sec", type=int, required=True)
    execute.add_argument("--ca-file", type=Path)
    execute.add_argument("--authorize-network-setup", action="store_true", required=True)
    args = parser.parse_args()
    if args.command == "plan":
        result = plan(args.task, args.output, args.download_timeout_sec)
    else:
        if args.timeout_sec <= 0:
            raise cache.CacheError("setup timeout must be positive")
        result = warm(args.plan, cache.Store(args.store), args.timeout_sec, args.ca_file)
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
