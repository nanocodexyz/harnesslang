import argparse
import contextlib
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import threading
import urllib.parse
import urllib.request

SCHEMA = "verifier-cache-v1"
BLOCK_BYTES = 1024 * 1024
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
DEFAULT_ROOT = Path.home() / ".cache" / "harnesslang" / "verifier-cache-v1"


class CacheError(ValueError):
    pass


def digest(value):
    if not isinstance(value, str) or SHA256.fullmatch(value) is None:
        raise CacheError("invalid sha256")
    return value


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def hash_file(path):
    result = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(BLOCK_BYTES), b""):
            result.update(block)
    return result.hexdigest()


def source_url(value):
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise CacheError("source URL must be credential-free HTTPS without query or fragment")
    return value


class SecureRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, newurl):
        source_url(newurl)
        return super().redirect_request(request, response, code, message, headers, newurl)


class Store:
    def __init__(self, root=DEFAULT_ROOT):
        self.root = Path(root).expanduser().resolve()
        if not self.root.is_relative_to(Path.home().resolve()):
            raise CacheError("cache root must be restart-durable under home")
        self.blobs = self.root / "sha256"
        self.blobs.mkdir(parents=True, exist_ok=True)

    def path(self, sha256):
        return self.blobs / digest(sha256)

    def verified(self, sha256):
        path = self.path(sha256)
        if not path.is_file() or path.is_symlink() or hash_file(path) != sha256:
            raise CacheError("missing or corrupt artifact " + sha256)
        return path

    def put(self, source, expected_sha256, bytes_max):
        digest(expected_sha256)
        if bytes_max <= 0:
            raise CacheError("bytes_max must be positive")
        descriptor, temporary = tempfile.mkstemp(prefix=".partial-", dir=self.blobs)
        result, size = hashlib.sha256(), 0
        try:
            with os.fdopen(descriptor, "wb") as destination:
                for block in iter(lambda: source.read(BLOCK_BYTES), b""):
                    size += len(block)
                    if size > bytes_max:
                        raise CacheError("artifact exceeds bytes_max")
                    result.update(block)
                    destination.write(block)
                destination.flush()
                os.fsync(destination.fileno())
            if result.hexdigest() != expected_sha256:
                raise CacheError("artifact digest mismatch")
            os.chmod(temporary, 0o444)
            try:
                os.link(temporary, self.path(expected_sha256))
            except FileExistsError:
                self.verified(expected_sha256)
            directory = os.open(self.blobs, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
            self.verified(expected_sha256)
            return {"sha256": expected_sha256, "bytes": size}
        finally:
            os.unlink(temporary)

    def put_file(self, path, expected_sha256, bytes_max):
        with open(path, "rb") as source:
            return self.put(source, expected_sha256, bytes_max)

    def fetch(self, url, expected_sha256, bytes_max, timeout_sec):
        source_url(url)
        if timeout_sec <= 0:
            raise CacheError("timeout_sec must be positive")
        if self.path(expected_sha256).exists():
            path = self.verified(expected_sha256)
            if path.stat().st_size > bytes_max:
                raise CacheError("cached artifact exceeds bytes_max")
            return {"sha256": expected_sha256, "bytes": path.stat().st_size, "cache_hit": True}
        opener = urllib.request.build_opener(SecureRedirect())
        with opener.open(url, timeout=timeout_sec) as response:
            return self.put(response, expected_sha256, bytes_max) | {"cache_hit": False}


def validate_manifest(manifest):
    if manifest.get("schema") != SCHEMA or manifest.get("transport") != "prewarmed-image+artifact-seed":
        raise CacheError("unsupported manifest")
    for field in ("task_digest", "verifier_sha256", "setup_sha256"):
        digest(manifest[field])
    for field in ("base_image_id", "warm_image_id"):
        value = manifest[field]
        if not isinstance(value, str) or not value.startswith("sha256:"):
            raise CacheError("image ID must be a sha256")
        digest(value[7:])
    digest(manifest["image_archive"]["sha256"])
    if manifest["image_archive"]["bytes"] <= 0:
        raise CacheError("empty image archive")
    pins = manifest["source_pins"]
    if not isinstance(pins, list) or not pins:
        raise CacheError("source pins required")
    for pin in pins:
        source_url(pin["url"])
        digest(pin["sha256"])
        if pin["bytes"] <= 0:
            raise CacheError("empty source pin")
    if manifest.get("all_arms_required") is not True or manifest.get("verifier_unchanged") is not True:
        raise CacheError("variant must declare unchanged verifier and identical all-arm use")
    if manifest.get("legacy_comparable") is not False:
        raise CacheError("new cache variant cannot silently compare legacy")
    if manifest.get("network_required") is not True:
        raise CacheError("unchanged installer requires HTTPS network")
    return manifest


def artifact_records(manifest):
    return [manifest["image_archive"]] + manifest["source_pins"]


def publish_variant(store, manifest):
    validate_manifest(manifest)
    for record in artifact_records(manifest):
        if store.verified(record["sha256"]).stat().st_size != record["bytes"]:
            raise CacheError("artifact size mismatch")
    value = canonical(manifest)
    return store.put(io.BytesIO(value), hashlib.sha256(value).hexdigest(), len(value))["sha256"]


def load_variant(store, variant_id, expected=None, verify_artifacts=True):
    path = store.verified(variant_id)
    manifest = validate_manifest(json.loads(path.read_bytes()))
    for key, value in (expected or {}).items():
        if manifest.get(key) != value:
            raise CacheError("variant pin mismatch: " + key)
    if verify_artifacts:
        for record in artifact_records(manifest):
            if store.verified(record["sha256"]).stat().st_size != record["bytes"]:
                raise CacheError("artifact size mismatch")
    return manifest


def image_for_variant(manifest, timeout_sec=30):
    validate_manifest(manifest)
    image = manifest["warm_image_id"]
    result = subprocess.run(["docker", "image", "inspect", "--format", "{{.Id}}", image], capture_output=True, text=True, timeout=timeout_sec, check=True)
    if result.stdout.strip() != image:
        raise CacheError("warm image ID mismatch")
    return image


def restore_image(store, manifest, timeout_sec):
    archive = store.verified(manifest["image_archive"]["sha256"])
    subprocess.run(["docker", "load", "--input", str(archive)], check=True, timeout=timeout_sec, stdout=subprocess.DEVNULL)
    return image_for_variant(manifest)


@contextlib.contextmanager
def mirror(store, bind="127.0.0.1", port=0):
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            requested = urllib.parse.urlsplit(self.path)
            sha256 = requested.path.removeprefix("/sha256/")
            if not requested.path.startswith("/sha256/") or requested.query or SHA256.fullmatch(sha256) is None:
                self.send_error(404)
                return
            try:
                path = store.verified(sha256)
            except CacheError:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(path.stat().st_size))
            self.send_header("ETag", '"' + sha256 + '"')
            self.end_headers()
            with path.open("rb") as source:
                shutil.copyfileobj(source, self.wfile, BLOCK_BYTES)

        def log_message(self, format, *args):
            pass

    server = http.server.HTTPServer((bind, port), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_address
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--store", type=Path, default=DEFAULT_ROOT)
    commands = parser.add_subparsers(dest="command", required=True)
    artifact = commands.add_parser("import")
    artifact.add_argument("path", type=Path)
    artifact.add_argument("sha256")
    artifact.add_argument("--bytes-max", type=int, required=True)
    fetch = commands.add_parser("fetch")
    fetch.add_argument("url")
    fetch.add_argument("sha256")
    fetch.add_argument("--bytes-max", type=int, required=True)
    fetch.add_argument("--timeout-sec", type=float, required=True)
    publish = commands.add_parser("publish")
    publish.add_argument("manifest", type=Path)
    readiness = commands.add_parser("ready")
    readiness.add_argument("variant_id")
    restore = commands.add_parser("restore")
    restore.add_argument("variant_id")
    restore.add_argument("--timeout-sec", type=float, required=True)
    args = parser.parse_args()
    store = Store(args.store)
    if args.command == "import":
        result = store.put_file(args.path, args.sha256, args.bytes_max)
    elif args.command == "fetch":
        result = store.fetch(args.url, args.sha256, args.bytes_max, args.timeout_sec)
    elif args.command == "publish":
        result = {"variant_id": publish_variant(store, json.loads(args.manifest.read_bytes()))}
    else:
        manifest = load_variant(store, args.variant_id)
        image = restore_image(store, manifest, args.timeout_sec) if args.command == "restore" else image_for_variant(manifest)
        result = {"ready": True, "variant_id": args.variant_id, "image_id": image, "network_required": True}
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
