import gzip
import hashlib
import io
import json
import os
import sys
import tarfile
import urllib.parse
import urllib.request

HUB_URL = "https://ofhuhcpkvzjlejydnvyd.supabase.co"
HUB_KEY = "sb_publishable_Z-vuQbpvpG-PStjbh4yE0Q_e-d3MTIH"
PAGE_ROWS = 1000
PACKAGE_FILES = ["instruction.md", "README.md", "task.toml"]
PACKAGE_DIRECTORIES = ["environment", "solution", "steps", "tests"]

DATASETS = {
    "terminal-bench-2.1": {
        "org": "terminal-bench",
        "name": "terminal-bench-2-1",
        "version": "7d7bdc1cbedad549fc1140404bd4dc45e5fd0ea7c4186773687d177ad3a0699a",
        "tasks": "a52279506fb02fe3b3daa430a0c8e027ad71e9df582b4bb833f709cb28b9252a",
    },
}


def get(url):
    request = urllib.request.Request(url, headers={"apikey": HUB_KEY})
    with urllib.request.urlopen(request) as response:
        return response.read()


def query(table, parameters):
    return json.loads(get(HUB_URL + "/rest/v1/" + table + "?" + urllib.parse.urlencode(parameters)))


def versions(dataset):
    found = query(
        "dataset_version",
        {
            "content_hash": "eq." + dataset["version"],
            "limit": "1",
            "package.name": "eq." + dataset["name"],
            "package.org.name": "eq." + dataset["org"],
            "package.type": "eq.dataset",
            "select": "id,package:package_id!inner(name,type,org:org_id!inner(name))",
        },
    )
    if not found:
        raise SystemExit("the Harbor registry has no version " + dataset["version"])
    rows = []
    offset = 0
    while True:
        page = query(
            "dataset_version_task",
            {
                "dataset_version_id": "eq." + found[0]["id"],
                "limit": str(PAGE_ROWS),
                "offset": str(offset),
                "order": "task_version_id",
                "select": "task_version:task_version_id(content_hash,archive_path,package:package_id(name))",
            },
        )
        rows.extend(row["task_version"] for row in page)
        if len(page) < PAGE_ROWS:
            break
        offset += PAGE_ROWS
    digest = hashlib.sha256(",".join(sorted(row["content_hash"] for row in rows)).encode()).hexdigest()
    if digest != dataset["tasks"]:
        raise SystemExit("tasks hash to " + digest + ", " + dataset["tasks"] + " is pinned")
    return {row["package"]["name"]: row for row in rows}


def checksum(files):
    packaged = sorted(
        path
        for path in files
        if path in PACKAGE_FILES or any(path.startswith(d + "/") for d in PACKAGE_DIRECTORIES)
    )
    lines = "".join(path + "\0" + hashlib.sha256(files[path]).hexdigest() + "\n" for path in packaged)
    return hashlib.sha256(lines.encode()).hexdigest()


def unpack(archive, destination, expected):
    files = {}
    modes = {}
    with tarfile.open(fileobj=io.BytesIO(gzip.decompress(archive))) as tar:
        for member in tar.getmembers():
            if member.isfile():
                path = member.name.removeprefix("./")
                files[path] = tar.extractfile(member).read()
                modes[path] = member.mode
    actual = checksum(files)
    if actual != expected:
        raise SystemExit(destination + " unpacks to " + actual + ", " + expected + " is pinned")
    for path, data in files.items():
        target = os.path.join(destination, path)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "wb") as handle:
            handle.write(data)
        os.chmod(target, modes[path] & 0o777)


def main():
    if len(sys.argv) < 3:
        raise SystemExit("usage: fetch.py DATASET OUT [--list | TASK...]")
    dataset, out = sys.argv[1], sys.argv[2]
    rows = versions(DATASETS[dataset])
    wanted = sys.argv[3:]
    if wanted == ["--list"]:
        for name in sorted(rows):
            print(name.split("/")[-1])
        return
    short = {name.split("/")[-1]: name for name in rows}
    for task in wanted or sorted(short):
        row = rows[short[task]]
        destination = os.path.join(out, task)
        if os.path.exists(os.path.join(destination, ".fetched")):
            continue
        archive = get(HUB_URL + "/storage/v1/object/packages/" + row["archive_path"])
        unpack(archive, destination, row["content_hash"])
        with open(os.path.join(destination, ".fetched"), "w") as handle:
            handle.write(row["content_hash"] + "\n")
        print(task, row["content_hash"])


main()
