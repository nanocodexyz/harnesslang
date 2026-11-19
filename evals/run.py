import argparse
import concurrent.futures
import hashlib
import json
import math
import shlex
import os
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import tomllib
import urllib.parse

import checkpoint
import checkpoint_phase
import checkpoint_meter
import provenance

ROOT = os.path.dirname(os.path.abspath(__file__))
AGENT_MOUNT = "/opt/nanoeval-agent"
JOURNAL_MOUNT = "/opt/nanoeval-journal"
CA_MOUNT = "/opt/nanoeval-ca.crt"
CA_ENV = ("SSL_CERT_FILE", "CURL_CA_BUNDLE", "REQUESTS_CA_BUNDLE", "GIT_SSL_CAINFO", "NODE_EXTRA_CA_CERTS")
METER_PORT = 18777
RELAY_PORT = 13128
DOCKER_BRIDGE = "bridge"
SETUP_TIMEOUT_SEC = 1800
METER_READY_ATTEMPTS = 50
METER_READY_SLEEP_SEC = 0.1
RUN_SHELL = 'if command -v bash >/dev/null 2>&1; then exec bash "$1"; else exec sh "$1"; fi'
AGENTS = ("nanocode", "hl-agent", "hl-agent-medium", "claude-code", "codex")
AGENT_BINARIES = {"hl-agent-medium": "hl-agent"}
AGENT_ARGS = {"hl-agent-medium": ["--effort", "medium"]}
AGENT_KEYS = {"nanocode": ("ANTHROPIC_API_KEY", "OPENAI_API_KEY", "NANOEVAL_CHATGPT_LOGIN"), "hl-agent": ("ANTHROPIC_API_KEY", "OPENAI_API_KEY", "NANOEVAL_CHATGPT_LOGIN"), "hl-agent-medium": ("ANTHROPIC_API_KEY", "OPENAI_API_KEY", "NANOEVAL_CHATGPT_LOGIN"), "claude-code": ("ANTHROPIC_API_KEY",), "codex": ("OPENAI_API_KEY", "NANOEVAL_CHATGPT_LOGIN")}
OPENAI_ROUTES = {"openai": "/openai", "chatgpt": "/chatgpt"}
CHATGPT_HALT_FILE = "chatgpt-halted.json"
METERED_KEY = "metered-by-host"
CLAUDE_MODEL_PREFIX = "claude-"
MODEL_ENDPOINTS = checkpoint_meter.MODEL_ENDPOINTS
MUSL_LOADER = "/lib/ld-musl-x86_64.so.1"
CLAUDE_CODE_SELECT = 'if [ -e ' + MUSL_LOADER + ' ]; then exec ' + AGENT_MOUNT + '/bin/claude-code-musl "$@"; else exec ' + AGENT_MOUNT + '/bin/claude-code "$@"; fi'
CONTAINER_RUN_DIGITS = 12
PACKAGE_FILES = ("instruction.md", "README.md", "task.toml")
PACKAGE_DIRECTORIES = ("environment", "solution", "steps", "tests")
PAUSE_FILE = "pause"
PAUSE_POLL_SEC = 15


def sh(args, timeout=SETUP_TIMEOUT_SEC, check=True, **kwargs):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout, **kwargs)
    if check and result.returncode != 0:
        raise RuntimeError(" ".join(args[:4]) + " exited " + str(result.returncode) + ": " + result.stderr[-2000:])
    return result


def load_suite(path, tasks_root):
    rows = []
    with open(path) as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            source, task = line.split()
            directory = os.path.join(tasks_root, source, task)
            if not os.path.isfile(os.path.join(directory, "task.toml")):
                raise SystemExit("suite names " + source + "/" + task + ", which " + tasks_root + " does not hold; run evals/setup.sh")
            rows.append({"source": source, "task": task, "dir": directory})
    return rows


def task_digest(directory):
    lines = []
    for base, _, names in os.walk(directory):
        for file in names:
            path = os.path.join(base, file)
            relative = os.path.relpath(path, directory)
            if relative in PACKAGE_FILES or relative.split(os.sep)[0] in PACKAGE_DIRECTORIES:
                lines.append(relative + "\0" + hashlib.sha256(open(path, "rb").read()).hexdigest() + "\n")
    return hashlib.sha256("".join(sorted(lines)).encode()).hexdigest()


def task_config(directory):
    with open(os.path.join(directory, "task.toml"), "rb") as handle:
        config = tomllib.load(handle)
    environment = config.get("environment", {})
    return {
        "image": environment.get("docker_image"),
        "cpus": environment.get("cpus", 1),
        "memory_mb": environment.get("memory_mb", 2048),
        "agent_timeout": float(config.get("agent", {}).get("timeout_sec", 600)),
        "verifier_timeout": float(config.get("verifier", {}).get("timeout_sec", 600)),
        "verifier_env": {key: str(value) for key, value in config.get("verifier", {}).get("env", {}).items()},
    }


def bridge_subnet():
    return sh(["docker", "network", "inspect", DOCKER_BRIDGE, "-f", "{{(index .IPAM.Config 0).Subnet}}"]).stdout.strip()


def network():
    gateway = sh(["docker", "network", "inspect", DOCKER_BRIDGE, "-f", "{{(index .IPAM.Config 0).Gateway}}"]).stdout.strip()
    proxy = urllib.parse.urlsplit(os.environ.get("HTTPS_PROXY") or os.environ.get("https_proxy") or "")
    upstream = socket.gethostbyname(proxy.hostname) + ":" + str(proxy.port) if proxy.hostname else None
    return gateway, upstream


def container_env(gateway, upstream, relay_port):
    env = {"no_proxy": gateway + ",localhost,127.0.0.1", "NO_PROXY": gateway + ",localhost,127.0.0.1"}
    if upstream:
        relay = "http://" + gateway + ":" + str(relay_port)
        for name in ("http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"):
            env[name] = relay
    return env


def agent_command(agent, model, workdir, run_url, instruction, max_turns, hl_agent_args, openai_route):
    openai_url = run_url + OPENAI_ROUTES[openai_route]
    if agent == "codex":
        return (
            {"CODEX_API_KEY": METERED_KEY, "OPENAI_BASE_URL": openai_url + "/v1", "HOME": JOURNAL_MOUNT + "/home"},
            [AGENT_MOUNT + "/bin/codex", "exec", "--dangerously-bypass-approvals-and-sandbox", "--skip-git-repo-check", "--json", "-m", model, "-C", workdir, "-c", 'openai_base_url="' + openai_url + '/v1"', instruction],
        )
    claude = model.startswith(CLAUDE_MODEL_PREFIX)
    if agent == "nanocode":
        return (
            {"NANOCODE_ANTHROPIC_BASE_URL": run_url + "/v1", "ANTHROPIC_API_KEY": "metered-by-host"} if claude else {"NANOCODE_OPENAI_BASE_URL": openai_url + "/v1", "OPENAI_API_KEY": METERED_KEY},
            [AGENT_MOUNT + "/bin/nanocode", "exec", "--model", model, "--home", JOURNAL_MOUNT + "/home", "--no-tempo-tools", "-C", workdir, instruction],
        )
    if agent == "claude-code":
        return (
            {"ANTHROPIC_BASE_URL": run_url, "ANTHROPIC_API_KEY": "metered-by-host", "HOME": JOURNAL_MOUNT + "/home", "IS_SANDBOX": "1", "DISABLE_AUTOUPDATER": "1", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"},
            ["sh", "-c", CLAUDE_CODE_SELECT, "sh", "-p", instruction, "--model", model, "--dangerously-skip-permissions", "--output-format", "stream-json", "--verbose"],
        )
    command = [AGENT_MOUNT + "/bin/hl-agent", "run", "--instruction", JOURNAL_MOUNT + "/instruction.md", "--cwd", workdir, "--model", model, "--journal", JOURNAL_MOUNT + "/hl"]
    if max_turns:
        command += ["--max-turns", str(max_turns)]
    command += AGENT_ARGS.get(agent, []) + hl_agent_args
    return ({"ANTHROPIC_BASE_URL": run_url, "ANTHROPIC_API_KEY": "metered-by-host"} if claude else {"OPENAI_BASE_URL": openai_url, "OPENAI_API_KEY": METERED_KEY}, command)


def run_agent(name, row, env, argv, timeout, journal, state=None):
    if state is None:
        raise checkpoint.ResumeError("run_agent requires a durable attempt")
    return checkpoint_phase.phase(sh, name, row, env, argv, timeout, journal, "agent", state)


def verify(name, row, config, out, state=None):
    if state is None:
        raise checkpoint.ResumeError("verify requires a durable attempt")
    journal = os.path.join(out, "journal")
    if state.value["stage"] != "verifier":
        sh(["docker", "exec", "-u", "0", name, "sh", "-c", "rm -rf /logs/verifier /tests && mkdir -p /logs/verifier && chmod 777 /logs/verifier"])
        sh(["docker", "cp", os.path.join(row["dir"], "tests") + "/.", name + ":/tests"])
    result = checkpoint_phase.phase(sh, name, row, config["verifier_env"],
        ["sh", "-c", RUN_SHELL, "sh", "/tests/test.sh"], config["verifier_timeout"], journal, "verifier", state)
    with open(os.path.join(out, "verifier.log"), "w") as handle:
        for suffix in ("stdout", "stderr"):
            path = os.path.join(journal, "verifier." + suffix)
            if os.path.isfile(path):
                with open(path, errors="replace") as source:
                    shutil.copyfileobj(source, handle)
    reward_dir = os.path.join(out, "verifier")
    if os.path.isdir(reward_dir):
        shutil.rmtree(reward_dir)
    sh(["docker", "cp", name + ":/logs/verifier", reward_dir], check=False)
    result["reward"] = None
    path = os.path.join(reward_dir, "reward.txt")
    if os.path.isfile(path):
        try:
            result["reward"] = float(open(path).read().strip())
        except ValueError:
            pass
    return result


def meter_totals(directory):
    totals = {"requests": 0, "ok_requests": 0, "input_tokens": 0, "output_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0, "cache_write_1h_tokens": 0, "reasoning_tokens": 0, "tool_calls": 0, "unknown_usage_requests": 0, "statuses": {}}
    _, records = checkpoint_meter.snapshot(directory, None)
    for record in records:
        if not record["path"].split("?")[0].endswith(MODEL_ENDPOINTS):
            continue
        totals["requests"] += 1
        status = str(record.get("status"))
        totals["statuses"][status] = totals["statuses"].get(status, 0) + 1
        if record.get("status") == 200:
            totals["ok_requests"] += 1
        usage = record.get("usage") or {}
        if not record.get("usage_known", False) or "input_tokens" not in usage or "output_tokens" not in usage:
            totals["unknown_usage_requests"] += 1
        for key in ("input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "cache_write_1h_tokens", "reasoning_tokens"):
            totals[key] += usage.get(key, 0)
        totals["tool_calls"] += len(record.get("tool_uses") or [])
    return totals


def outcome(agent_result, verifier, totals):
    if totals["ok_requests"] == 0:
        return "errored", "no model request succeeded"
    if verifier["reward"] is None:
        return "errored", "verifier wrote no reward" + (" (timed out)" if verifier["timed_out"] else "")
    return ("passed" if verifier["reward"] == 1 else "failed"), ("agent timed out" if agent_result["timed_out"] else None)


def record_crash(row, args, runs, error):
    out = os.path.join(runs, row["id"])
    path = os.path.join(out, "infrastructure.json")
    result = {key: row[key] for key in ("id", "source", "task", "agent", "attempt")}
    result.update(model=args.model, outcome="errored", infrastructure_status="unresumable" if isinstance(error, checkpoint.Unresumable) else "blocked", retry_policy="no_fresh_attempt", accounting_status="unknown_after_infrastructure",
        note="exception: " + type(error).__name__ + ": " + str(error)[-1500:])
    try:
        result["meter"] = meter_totals(out)
    except (OSError, ValueError, KeyError, TypeError):
        result["meter"] = {}
    result["meter"].update(accounting_status="unknown_after_infrastructure", unknown_requests=None)
    if os.path.isdir(out) and not os.path.exists(path):
        checkpoint.atomic(path, result, True)
    return result


def prepare_container(row, args, env, agent_root, journal, name, state):
    image = state.value["image_id"]
    if state.value.get("container_prepared"):
        return
    inspected = sh(["docker", "inspect", name], check=False)
    if inspected.returncode == 0:
        raise checkpoint.ResumeError("container creation interrupted; preserved for inspection")
    checkpoint.durable_directory(journal)
    checkpoint.durable_directory(os.path.join(os.path.dirname(journal), "meter-intents"))
    os.chmod(journal, 0o777)
    instruction = open(os.path.join(row["dir"], "instruction.md")).read()
    instruction_path = os.path.join(journal, "instruction.md")
    if not os.path.exists(instruction_path):
        with open(instruction_path, "x") as handle:
            handle.write(instruction)
            handle.flush()
            os.fsync(handle.fileno())
        checkpoint.sync_directory(journal)
    run = ["docker", "run", "-d", "--init", "--name", name]
    config = task_config(row["dir"])
    if args.limits == "task":
        run += ["--cpus", str(config["cpus"]), "--memory", str(config["memory_mb"] + args.agent_memory_mb) + "m"]
    for key, value in env.items():
        run += ["-e", key + "=" + value]
    if args.ca_bundle:
        run += ["-v", os.path.abspath(args.ca_bundle) + ":" + CA_MOUNT + ":ro"]
        for key in CA_ENV:
            run += ["-e", key + "=" + CA_MOUNT]
    run += ["-v", agent_root + ":" + AGENT_MOUNT + ":ro", "-v", journal + ":" + JOURNAL_MOUNT, "--entrypoint", "", image, "tail", "-f", "/dev/null"]
    sh(run)
    state.value["container_prepared"] = True
    state.save()


def run_row(row, args, gateway, env, lock, agent_root, runs):
    out = os.path.join(runs, row["id"])
    state = checkpoint.Attempt(out, dict(campaign=args.campaign_identity, row=row["id"]))
    done = state.completed()
    if done:
        return done
    if os.path.exists(os.path.join(out, "infrastructure.json")):
        return checkpoint.read(os.path.join(out, "infrastructure.json"))
    if os.path.isfile(os.path.join(runs, CHATGPT_HALT_FILE)):
        raise checkpoint.ResumeError("meter halt retained; no fallback or new attempt")
    while os.path.exists(os.path.join(os.path.dirname(runs), PAUSE_FILE)):
        time.sleep(PAUSE_POLL_SEC)
    config = task_config(row["dir"])
    journal = os.path.join(out, "journal")
    name = "nanoeval-" + hashlib.sha256(runs.encode()).hexdigest()[:CONTAINER_RUN_DIGITS] + "-" + row["id"]
    result = {key: row[key] for key in ("id", "source", "task", "agent", "attempt")}
    result["model"] = args.model
    result["cache"] = getattr(args, "cache_identity", {"enabled": False})
    try:
        if "image_id" not in state.value:
            state.value.update(image_id=args.image_ids[row["id"]], workdir=args.workdirs[row["id"]], accounting_status="observed")
            state.save()
        incarnation = getattr(args, "meter_incarnation", "mock-offline")
        if state.value.get("meter_incarnation", incarnation) != incarnation:
            state.value["accounting_status"] = "unknown_meter_incarnation_changed"
        state.value["meter_incarnation"] = incarnation
        state.save()
        row["workdir"] = state.value["workdir"]
        prepare_container(row, args, env, agent_root, journal, name, state)
        info = checkpoint_phase.validate_container(sh, name, state.value, journal, agent_root)
        if state.value["stage"] == "prepared" and not info["State"]["Running"]:
            sh(["docker", "start", name])
        if state.value["stage"] in ("prepared", "agent") and "agent_result" not in state.value:
            instruction = open(os.path.join(journal, "instruction.md")).read()
            run_url = "http://" + (args.meter_host or gateway) + ":" + str(args.meter_port) + "/run/" + row["id"]
            agent_env, argv = agent_command(row["agent"], args.model, row["workdir"], run_url, instruction, args.max_turns, args.hl_agent_arg or [], args.openai_route)
            state.value["agent_result"] = run_agent(name, row, agent_env, argv, config["agent_timeout"], journal, state)
            state.save()
        if state.value["stage"] == "agent":
            info = checkpoint_phase.container(sh, name)
            if not info["State"]["Running"]:
                sh(["docker", "start", name])
                state.value["container_restarted_after_agent"] = True
                state.save()
        verifier = verify(name, row, config, out, state)
        ledger = checkpoint_meter.reconcile(out, incarnation)
        totals = meter_totals(out)
        totals["durable_ledger"] = ledger
        if ledger["unknown"] or ledger["unmatched_records"]:
            state.value["accounting_status"] = "unknown_meter_requests"
        if totals["unknown_usage_requests"]:
            state.value["accounting_status"] = "unknown_model_usage"
        totals["accounting_status"] = state.value["accounting_status"]
        totals["unknown_requests"] = None if state.value["accounting_status"] != "observed" else 0
        kind, note = outcome(state.value["agent_result"], verifier, totals)
        result.update(outcome=kind, note=note, agent_result=state.value["agent_result"], verifier=verifier,
            meter=totals, accounting_status=state.value["accounting_status"], image=state.value["image_id"], workdir=row["workdir"])
        usage = os.path.join(journal, "hl", "usage.json")
        if os.path.isfile(usage):
            result["self_reported_usage"] = checkpoint.read(usage)
        result = state.complete(result)
        sh(["docker", "stop", "--time", "1", name], check=False)
        return result
    except (RuntimeError, subprocess.TimeoutExpired, OSError, ValueError) as error:
        state.value["accounting_status"] = "unknown_after_infrastructure"
        state.save()
        return record_crash(row, args, runs, error)


def start_services(gateway, upstream, runs, logs, meter_port, relay_port, meter_host):
    processes = []
    owner = checkpoint.digest([os.path.abspath(runs), checkpoint.read(os.path.join(logs, "campaign.json"))["identity"]])
    state_root = os.path.join(logs, "services")
    checkpoint.durable_directory(state_root)
    meter_env = dict(os.environ)
    if meter_host != gateway:
        meter_env["NANOEVAL_METER_CLIENTS"] = bridge_subnet()
    specs = [("meter", [meter_host, str(meter_port), runs], meter_env)]
    if upstream:
        specs.append(("relay", [gateway, str(relay_port), upstream], dict(os.environ)))
    try:
        for role, arguments, environment in specs:
            relevant = ("ANTHROPIC_API_KEY", "OPENAI_API_KEY", "NANOEVAL_CHATGPT_LOGIN", "HTTPS_PROXY", "https_proxy", "NANOEVAL_METER_CLIENTS", "NANOEVAL_ANTHROPIC_UPSTREAM", "NANOEVAL_OPENAI_UPSTREAM", "NANOEVAL_CHATGPT_UPSTREAM")
            environment_sha256 = checkpoint.digest({key: environment.get(key) for key in relevant})
            command = [sys.executable, os.path.join(ROOT, "checkpoint_meter.py"), "--role", role, "--owner", owner,
                "--environment-sha256", environment_sha256, "--state", os.path.join(state_root, role + ".json")] + arguments
            with open(os.path.join(logs, role + ".log"), "a") as log:
                endpoint = (meter_host, meter_port) if role == "meter" else (gateway, relay_port)
                processes.append(checkpoint_phase.service(command, log, environment, endpoint))
        for _ in range(METER_READY_ATTEMPTS):
            if any(process.poll() is not None for process in processes):
                raise checkpoint.ResumeError("campaign meter or relay exited; see " + logs)
            try:
                socket.create_connection((meter_host, meter_port), timeout=1).close()
                time.sleep(METER_READY_SLEEP_SEC)
                if all(process.poll() is None for process in processes):
                    return processes
            except OSError:
                time.sleep(METER_READY_SLEEP_SEC)
        raise checkpoint.ResumeError("campaign meter did not start; see " + logs)
    except BaseException:
        checkpoint_phase.stop_services(processes)
        raise


def matrix(args, agents):
    if args.attempts <= 0 or args.parallel <= 0 or len(set(agents)) != len(agents):
        raise checkpoint.ResumeError("attempts and parallel must be positive; agents unique")
    tasks = load_suite(args.suite, args.tasks)
    if args.only:
        missing = set(args.only) - {task["task"] for task in tasks}
        if missing:
            raise checkpoint.ResumeError("unknown selected task")
        tasks = [task for task in tasks if task["task"] in args.only]
    tasks.sort(key=lambda task: -task_config(task["dir"])["agent_timeout"])
    rows = []
    for task in tasks:
        for attempt in range(1, args.attempts + 1):
            for agent in agents:
                row = dict(task, agent=agent, attempt=attempt)
                row["id"] = task["source"] + "." + task["task"] + "." + agent + "." + str(attempt)
                if any(character not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-" for character in row["id"]):
                    raise checkpoint.ResumeError("unsafe row identifier")
                rows.append(row)
    if not rows or len({row["id"] for row in rows}) != len(rows):
        raise checkpoint.ResumeError("matrix must be nonempty and unique")
    return rows



def preflight_rows(args, rows, identity):
    root = os.path.join(args.out, "rows")
    expected = {row["id"] for row in rows}
    campaign_hash = checkpoint.digest(identity)
    marker = os.path.join(args.out, "matrix-prepared.json")
    prepared = os.path.exists(marker)
    if prepared and checkpoint.read(marker) != {"campaign": campaign_hash, "rows": sorted(expected)}:
        raise checkpoint.ResumeError("matrix preparation identity changed")
    if not os.path.isdir(root):
        if prepared:
            raise checkpoint.ResumeError("prepared matrix lost row directory")
        return
    actual = {name for name in os.listdir(root) if os.path.isdir(os.path.join(root, name))}
    if actual - expected:
        raise checkpoint.ResumeError("unexpected rows outside campaign matrix")
    if prepared and actual != expected:
        raise checkpoint.ResumeError("prepared matrix lost rows; no fresh attempts permitted")
    for name in sorted(actual):
        directory = os.path.join(root, name)
        path = os.path.join(directory, "checkpoint.json")
        if not os.path.exists(path):
            raise checkpoint.ResumeError("row lost checkpoint; no fresh attempt permitted")
        value = checkpoint.read(path)
        if value["identity"] != {"campaign": campaign_hash, "row": name} or value["stage"] not in checkpoint.STAGES:
            raise checkpoint.ResumeError("row checkpoint identity mismatch")
        if not prepared and value["stage"] != "prepared":
            raise checkpoint.ResumeError("dispatch without prepared matrix")
        result_path = os.path.join(directory, "result.json")
        if value["stage"] == "completed":
            if not os.path.exists(result_path) or checkpoint.digest(checkpoint.read(result_path)) != value["result_sha256"]:
                raise checkpoint.ResumeError("completed row result changed or missing")


def prepare_matrix(args, rows, campaign_hash):
    root = os.path.join(args.out, "rows")
    checkpoint.durable_directory(root)
    for row in rows:
        checkpoint.Attempt(os.path.join(root, row["id"]), {"campaign": campaign_hash, "row": row["id"]})
    marker = os.path.join(args.out, "matrix-prepared.json")
    if not os.path.exists(marker):
        checkpoint.atomic(marker, {"campaign": campaign_hash, "rows": sorted(row["id"] for row in rows)}, True)

def cache_variants(args, rows, images, workdirs):
    selected = {}
    for specification in getattr(args, "cache_variant", None) or []:
        task, separator, variant = specification.partition("=")
        if not separator or not task or task in selected:
            raise checkpoint.ResumeError("cache variant must be unique source/task=sha256")
        selected[task] = variant
    args.cache_identity = {"enabled": False}
    if not selected:
        return
    import cache_store
    store = cache_store.Store()
    tasks = {row["source"] + "/" + row["task"]: row for row in rows}
    if selected.keys() - tasks.keys():
        raise checkpoint.ResumeError("cache variant task outside campaign")
    variants = {}
    for key, variant in sorted(selected.items()):
        row = tasks[key]
        expected = {"task_digest": task_digest(row["dir"]), "verifier_sha256": provenance.file_digest(os.path.join(row["dir"], "tests", "test.sh")), "base_image_id": images[row["id"]]}
        manifest = cache_store.load_variant(store, variant, expected=expected)
        image = cache_store.image_for_variant(manifest)
        inspected = json.loads(sh(["docker", "image", "inspect", image]).stdout)[0]
        if inspected["Id"] != image:
            raise checkpoint.ResumeError("cache warm image identity mismatch")
        variants[key] = {"variant_id": variant, "manifest": manifest}
        for item in rows:
            if item["source"] + "/" + item["task"] == key:
                images[item["id"]] = image
                workdirs[item["id"]] = inspected["Config"].get("WorkingDir") or "/"
    args.cache_identity = {"enabled": True, "variants": variants, "new_infrastructure_campaign": True}


def preflight(args, rows):
    images, workdirs, configs = {}, {}, {}
    inspected, missing = {}, set()
    for row in rows:
        config = task_config(row["dir"])
        if any(not math.isfinite(config[key]) or config[key] <= 0 for key in ("agent_timeout", "verifier_timeout")):
            raise checkpoint.ResumeError("invalid timeout")
        configs[row["id"]] = config
        image = config["image"] or "nanoeval-eval/" + row["source"] + "-" + row["task"] + ":" + task_digest(row["dir"])[:16]
        if image in missing:
            continue
        if image not in inspected:
            result = sh(["docker", "image", "inspect", image], check=False)
            if result.returncode:
                missing.add(image)
                continue
            inspected[image] = json.loads(result.stdout)[0]
        images[row["id"]] = inspected[image]["Id"]
        workdirs[row["id"]] = inspected[image]["Config"].get("WorkingDir") or "/"
    if missing:
        raise checkpoint.ResumeError("matrix " + str(len(rows)) + " rows; missing " + str(len(missing)) + " images: " + ", ".join(sorted(missing)) + "; no implicit pull/build")
    cache_variants(args, rows, images, workdirs)
    identity = provenance.campaign(args, rows, task_digest, configs, images)
    path = os.path.join(args.out, "campaign.json")
    if os.path.exists(path) and checkpoint.read(path)["identity"] != identity:
        raise checkpoint.ResumeError("campaign identity mismatch; no work dispatched")
    preflight_rows(args, rows, identity)
    args.image_ids, args.workdirs = images, workdirs
    return identity


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--suite", required=True)
    parser.add_argument("--tasks", required=True)
    parser.add_argument("--agent-root", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--agents", default=",".join(AGENTS))
    parser.add_argument("--attempts", type=int, required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--parallel", type=int, required=True)
    parser.add_argument("--limits", choices=("task", "none"), required=True)
    parser.add_argument("--agent-memory-mb", type=int, default=0)
    parser.add_argument("--max-turns", type=int)
    parser.add_argument("--meter-port", type=int, default=METER_PORT)
    parser.add_argument("--relay-port", type=int, default=RELAY_PORT)
    parser.add_argument("--only", action="append")
    parser.add_argument("--ca-bundle")
    parser.add_argument("--hl-agent-arg", action="append")
    parser.add_argument("--openai-route", choices=tuple(OPENAI_ROUTES), default="openai")
    parser.add_argument("--meter-host")
    parser.add_argument("--cache-variant", action="append", metavar="SOURCE/TASK=SHA256")
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--preflight", action="store_true")
    args = parser.parse_args()
    agents = args.agents.split(",")
    for agent in agents:
        if agent not in AGENTS:
            raise SystemExit("unknown agent " + agent)
        binary = os.path.join(args.agent_root, "bin", AGENT_BINARIES.get(agent, agent))
        if not os.access(binary, os.X_OK):
            raise SystemExit(binary + " is missing; run evals/setup.sh")
    rows = matrix(args, agents)
    identity = preflight(args, rows)
    print("matrix", len(rows), "identity", checkpoint.digest(identity), flush=True)
    if args.preflight:
        return
    for agent in agents:
        if not any(os.environ.get(key) for key in AGENT_KEYS[agent]):
            raise SystemExit(agent + " needs a configured meter credential")
    campaign = checkpoint.Campaign(os.path.abspath(args.out), identity, args.resume)
    args.campaign_identity = campaign.identity
    prepare_matrix(args, rows, campaign.identity)
    runs = os.path.abspath(os.path.join(args.out, "rows"))
    checkpoint.durable_directory(runs)
    resume_argv = [value for value in sys.argv[1:] if value != "--resume"] + ["--resume"]
    print("resume:", shlex.join([sys.executable, os.path.abspath(__file__)] + resume_argv), flush=True)
    services = []
    try:
        gateway, upstream = network()
        env = container_env(gateway, upstream, args.relay_port)
        services = start_services(gateway, upstream, runs, os.path.abspath(args.out), args.meter_port, args.relay_port, args.meter_host or gateway)
        args.meter_incarnation = checkpoint.read(os.path.join(args.out, "services", "meter.json"))["incarnation"]
        lock = threading.Lock()
        with concurrent.futures.ThreadPoolExecutor(args.parallel) as pool:
            futures = {pool.submit(run_row, row, args, gateway, env, lock, os.path.abspath(args.agent_root), runs): row for row in rows}
            for future in concurrent.futures.as_completed(futures):
                try:
                    result = future.result()
                except Exception as error:
                    result = record_crash(futures[future], args, runs, error)
                print(result["id"], result["outcome"], result.get("note") or "", flush=True)
                if os.path.isfile(os.path.join(runs, CHATGPT_HALT_FILE)):
                    cancelled = sum(1 for pending in futures if pending.cancel())
                    print("the meter halted the ChatGPT route; cancelled " + str(cancelled) + " rows", flush=True)
    finally:
        checkpoint_phase.stop_services(services)
        campaign.close()


if __name__ == "__main__":
    main()
