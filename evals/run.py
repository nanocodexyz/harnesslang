import argparse
import concurrent.futures
import hashlib
import json
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

ROOT = os.path.dirname(os.path.abspath(__file__))
AGENT_MOUNT = "/opt/nanoeval-agent"
JOURNAL_MOUNT = "/opt/nanoeval-journal"
METER_PORT = 18777
RELAY_PORT = 13128
DOCKER_BRIDGE = "bridge"
KILL_GRACE_SEC = 10
SETUP_TIMEOUT_SEC = 1800
METER_READY_ATTEMPTS = 50
METER_READY_SLEEP_SEC = 0.1
RUN_SHELL = 'if command -v bash >/dev/null 2>&1; then exec bash "$1"; else exec sh "$1"; fi'
AGENTS = ("nanocode", "hl-agent")
PACKAGE_FILES = ("instruction.md", "README.md", "task.toml")
PACKAGE_DIRECTORIES = ("environment", "solution", "steps", "tests")


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


def image_for(task, config, env, lock):
    if config["image"]:
        with lock:
            if sh(["docker", "image", "inspect", config["image"]], check=False).returncode != 0:
                sh(["docker", "pull", "-q", config["image"]])
        return config["image"]
    digest = task_digest(task["dir"])
    tag = "nanoeval-eval/" + task["source"] + "-" + task["task"] + ":" + digest[:16]
    with lock:
        if sh(["docker", "image", "inspect", tag], check=False).returncode != 0:
            build = ["docker", "build", "-q", "-t", tag]
            for name, value in env.items():
                build += ["--build-arg", name + "=" + value]
            sh(build + [os.path.join(task["dir"], "environment")])
    return tag


def agent_command(agent, model, workdir, run_url, instruction, max_turns):
    if agent == "nanocode":
        return (
            {"NANOCODE_ANTHROPIC_BASE_URL": run_url + "/v1", "ANTHROPIC_API_KEY": "metered-by-host"},
            [AGENT_MOUNT + "/bin/nanocode", "exec", "--model", model, "--home", JOURNAL_MOUNT + "/home", "--no-tempo-tools", "-C", workdir, instruction],
        )
    command = [AGENT_MOUNT + "/bin/hl-agent", "run", "--instruction", JOURNAL_MOUNT + "/instruction.md", "--cwd", workdir, "--model", model, "--journal", JOURNAL_MOUNT + "/hl"]
    if max_turns:
        command += ["--max-turns", str(max_turns)]
    return ({"ANTHROPIC_BASE_URL": run_url, "ANTHROPIC_API_KEY": "metered-by-host"}, command)


def run_agent(name, row, env, argv, timeout, journal):
    exec_env = []
    for key, value in env.items():
        exec_env += ["-e", key + "=" + value]
    wrapped = ["sh", "-c", 'echo $$ > ' + JOURNAL_MOUNT + '/agent.pid; exec "$@"', "sh"] + argv
    started = time.time()
    with open(os.path.join(journal, "agent.stdout"), "w") as out, open(os.path.join(journal, "agent.stderr"), "w") as err:
        process = subprocess.Popen(["docker", "exec", "-w", row["workdir"]] + exec_env + [name] + wrapped, stdout=out, stderr=err)
        try:
            code = process.wait(timeout=timeout)
            timed_out = False
        except subprocess.TimeoutExpired:
            timed_out = True
            pid = open(os.path.join(journal, "agent.pid")).read().strip()
            sh(["docker", "exec", "-u", "0", name, "kill", "-9", pid], check=False)
            try:
                code = process.wait(timeout=KILL_GRACE_SEC)
            except subprocess.TimeoutExpired:
                process.kill()
                code = process.wait()
    return {"exit": code, "timed_out": timed_out, "wall_ms": int((time.time() - started) * 1000)}


def verify(name, row, config, out):
    sh(["docker", "exec", "-u", "0", name, "sh", "-c", "rm -rf /logs/verifier /tests && mkdir -p /logs/verifier && chmod 777 /logs/verifier"])
    sh(["docker", "cp", os.path.join(row["dir"], "tests") + "/.", name + ":/tests"])
    env = []
    for key, value in config["verifier_env"].items():
        env += ["-e", key + "=" + value]
    started = time.time()
    try:
        result = subprocess.run(
            ["docker", "exec", "-w", row["workdir"]] + env + [name, "sh", "-c", RUN_SHELL, "sh", "/tests/test.sh"],
            capture_output=True, text=True, timeout=config["verifier_timeout"],
        )
        code, timed_out = result.returncode, False
        log = result.stdout + result.stderr
    except subprocess.TimeoutExpired as expired:
        code, timed_out = None, True
        log = (expired.stdout or b"").decode(errors="replace") if isinstance(expired.stdout, bytes) else (expired.stdout or "")
    with open(os.path.join(out, "verifier.log"), "w") as handle:
        handle.write(log)
    sh(["docker", "cp", name + ":/logs/verifier", os.path.join(out, "verifier")], check=False)
    reward = None
    path = os.path.join(out, "verifier", "reward.txt")
    if os.path.isfile(path):
        try:
            reward = float(open(path).read().strip())
        except ValueError:
            reward = None
    return {"exit": code, "timed_out": timed_out, "reward": reward, "wall_ms": int((time.time() - started) * 1000)}


def meter_totals(directory):
    totals = {"requests": 0, "ok_requests": 0, "input_tokens": 0, "output_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0, "cache_write_1h_tokens": 0, "tool_calls": 0, "statuses": {}}
    path = os.path.join(directory, "requests.jsonl")
    if not os.path.isfile(path):
        return totals
    for line in open(path):
        record = json.loads(line)
        if not record["path"].endswith("/messages"):
            continue
        totals["requests"] += 1
        status = str(record.get("status"))
        totals["statuses"][status] = totals["statuses"].get(status, 0) + 1
        if record.get("status") == 200:
            totals["ok_requests"] += 1
        for key in ("input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "cache_write_1h_tokens"):
            totals[key] += record["usage"].get(key, 0)
        totals["tool_calls"] += len(record.get("tool_uses") or [])
    return totals


def outcome(agent_result, verifier, totals):
    if totals["ok_requests"] == 0:
        return "errored", "no model request succeeded"
    if verifier["reward"] is None:
        return "errored", "verifier wrote no reward" + (" (timed out)" if verifier["timed_out"] else "")
    return ("passed" if verifier["reward"] == 1 else "failed"), ("agent timed out" if agent_result["timed_out"] else None)


def run_row(row, args, gateway, env, lock, agent_root, runs):
    out = os.path.join(runs, row["id"])
    if os.path.isfile(os.path.join(out, "result.json")):
        return json.load(open(os.path.join(out, "result.json")))
    if os.path.isdir(out):
        shutil.rmtree(out)
    journal = os.path.join(out, "journal")
    os.makedirs(journal)
    os.chmod(journal, 0o777)
    config = task_config(row["dir"])
    name = "nanoeval-" + row["id"]
    result = {"id": row["id"], "source": row["source"], "task": row["task"], "agent": row["agent"], "attempt": row["attempt"], "model": args.model}
    try:
        image = image_for(row, config, env, lock)
        workdir = sh(["docker", "image", "inspect", "-f", "{{.Config.WorkingDir}}", image]).stdout.strip() or "/"
        row["workdir"] = workdir
        sh(["docker", "rm", "-f", name], check=False)
        run = ["docker", "run", "-d", "--init", "--name", name]
        if args.limits == "task":
            run += ["--cpus", str(config["cpus"]), "--memory", str(config["memory_mb"] + args.agent_memory_mb) + "m"]
        for key, value in env.items():
            run += ["-e", key + "=" + value]
        run += ["-v", agent_root + ":" + AGENT_MOUNT + ":ro", "-v", journal + ":" + JOURNAL_MOUNT, "--entrypoint", "", image, "tail", "-f", "/dev/null"]
        sh(run)
        instruction = open(os.path.join(row["dir"], "instruction.md")).read()
        with open(os.path.join(journal, "instruction.md"), "w") as handle:
            handle.write(instruction)
        run_url = "http://" + gateway + ":" + str(args.meter_port) + "/run/" + row["id"]
        agent_env, argv = agent_command(row["agent"], args.model, workdir, run_url, instruction, args.max_turns)
        agent_result = run_agent(name, row, agent_env, argv, config["agent_timeout"], journal)
        peak = sh(["docker", "exec", name, "cat", "/sys/fs/cgroup/memory.peak"], check=False).stdout.strip()
        verifier = verify(name, row, config, out)
        totals = meter_totals(out)
        kind, note = outcome(agent_result, verifier, totals)
        result.update({"outcome": kind, "note": note, "agent": row["agent"], "agent_result": agent_result, "verifier": verifier, "meter": totals, "memory_peak_bytes": int(peak) if peak.isdigit() else None, "image": image, "workdir": workdir})
        usage = os.path.join(journal, "hl", "usage.json")
        if os.path.isfile(usage):
            result["self_reported_usage"] = json.load(open(usage))
    except (RuntimeError, subprocess.TimeoutExpired, OSError) as error:
        result.update({"outcome": "errored", "note": "infrastructure: " + str(error)[-1500:], "meter": meter_totals(out)})
    finally:
        sh(["docker", "exec", "-u", "0", name, "chown", "-R", str(os.getuid()) + ":" + str(os.getgid()), JOURNAL_MOUNT], check=False)
        sh(["docker", "rm", "-f", name], check=False)
    with open(os.path.join(out, "result.json"), "w") as handle:
        json.dump(result, handle, indent=1)
    return result


def start_services(gateway, upstream, runs, logs, meter_port, relay_port):
    processes = []
    meter_log = open(os.path.join(logs, "meter.log"), "a")
    processes.append(subprocess.Popen([sys.executable, os.path.join(ROOT, "meter.py"), gateway, str(meter_port), runs], stdout=meter_log, stderr=meter_log, start_new_session=True))
    if upstream:
        relay_log = open(os.path.join(logs, "relay.log"), "a")
        processes.append(subprocess.Popen([sys.executable, os.path.join(ROOT, "relay.py"), gateway, str(relay_port), upstream], stdout=relay_log, stderr=relay_log, start_new_session=True))
    for _ in range(METER_READY_ATTEMPTS):
        exited = [process for process in processes if process.poll() is not None]
        if exited:
            raise SystemExit("the meter or relay exited (is its port taken?); see " + logs)
        try:
            socket.create_connection((gateway, meter_port), timeout=1).close()
            time.sleep(METER_READY_SLEEP_SEC)
            if all(process.poll() is None for process in processes):
                return processes
        except OSError:
            time.sleep(METER_READY_SLEEP_SEC)
        except OSError:
            time.sleep(METER_READY_SLEEP_SEC)
    raise SystemExit("the meter did not start; see " + logs + "/meter.log")


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
    args = parser.parse_args()
    agents = args.agents.split(",")
    for agent in agents:
        if agent not in AGENTS:
            raise SystemExit("unknown agent " + agent)
        binary = os.path.join(args.agent_root, "bin", agent)
        if not os.access(binary, os.X_OK):
            raise SystemExit(binary + " is missing; run evals/setup.sh")
    if not os.environ.get("ANTHROPIC_API_KEY"):
        raise SystemExit("ANTHROPIC_API_KEY is not set")
    tasks = load_suite(args.suite, args.tasks)
    if args.only:
        tasks = [task for task in tasks if task["task"] in args.only]
    runs = os.path.abspath(os.path.join(args.out, "rows"))
    os.makedirs(runs, exist_ok=True)
    rows = []
    for attempt in range(1, args.attempts + 1):
        for task in tasks:
            for agent in agents:
                row = dict(task, agent=agent, attempt=attempt)
                row["id"] = task["source"] + "." + task["task"] + "." + agent + "." + str(attempt)
                rows.append(row)
                os.makedirs(os.path.join(runs, row["id"]), exist_ok=True)
    with open(os.path.join(args.out, "run.json"), "w") as handle:
        json.dump({"argv": sys.argv[1:], "rows": len(rows), "started": time.time()}, handle, indent=1)
    gateway, upstream = network()
    env = container_env(gateway, upstream, args.relay_port)
    services = start_services(gateway, upstream, runs, os.path.abspath(args.out), args.meter_port, args.relay_port)
    lock = threading.Lock()
    try:
        with concurrent.futures.ThreadPoolExecutor(args.parallel) as pool:
            futures = {pool.submit(run_row, row, args, gateway, env, lock, os.path.abspath(args.agent_root), runs): row for row in rows}
            for future in concurrent.futures.as_completed(futures):
                result = future.result()
                print(result["id"], result["outcome"], result.get("note") or "", flush=True)
    finally:
        for process in services:
            os.killpg(process.pid, signal.SIGTERM)


main()
