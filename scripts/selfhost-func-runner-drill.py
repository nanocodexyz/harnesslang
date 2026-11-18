import argparse
import importlib.util
import json
import os
import pathlib
import signal
import sys
import time

DRILL_COMMAND_SECONDS_MAX = 3
DRILL_TIMEOUT_SECONDS_MAX = 1
DRILL_OUTPUT_BYTES_MAX = 64
DRILL_SLEEP_SECONDS = 30
DRILL_READY_POLLS_MAX = 300
DRILL_POLL_SECONDS = 0.01
ROOT = pathlib.Path(__file__).resolve().parent.parent


def running(pid):
    path = pathlib.Path(f"/proc/{pid}/stat")
    try:
        record = path.read_text()
    except FileNotFoundError:
        return False
    state = record.rsplit(")", 1)[1].split()[0]
    return state not in ("Z", "X")


def load_runner():
    spec = importlib.util.spec_from_file_location("function_parity_runner", ROOT / "scripts/selfhost-func.py")
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    return runner


def command_case(runner, directory, name, code, seconds, expected):
    log = directory / f"{name}.log"
    error = None
    start = time.monotonic()
    try:
        runner.bounded_run([sys.executable, "-c", code], log, (0,), seconds, DRILL_OUTPUT_BYTES_MAX)
    except RuntimeError as failure:
        error = str(failure)
    elapsed = time.monotonic() - start
    if expected is None:
        if error is not None:
            raise RuntimeError(f"positive drill rejected: {name}: {error}")
    elif error is None or expected not in error:
        raise RuntimeError(f"drill failed to catch expected policy: {name}: {error}")
    stdout_size = log.stat().st_size
    stderr_size = log.with_suffix(log.suffix + ".err").stat().st_size
    if stdout_size + stderr_size > DRILL_OUTPUT_BYTES_MAX:
        raise RuntimeError(f"drill wrote past output policy: {name}")
    return {"case": name, "expected": expected or "completed", "actual": error or "completed", "stdout_bytes": stdout_size, "stderr_bytes": stderr_size, "wall_seconds": elapsed}


def background_case(runner, directory):
    pid_path = directory / "background.pid"
    ready_path = directory / "background.ready"
    child = f"import pathlib,time; pathlib.Path({str(ready_path)!r}).write_text('ready'); time.sleep({DRILL_SLEEP_SECONDS})"
    parent = "\n".join([
        "import pathlib,subprocess,sys,time",
        f"child=subprocess.Popen([sys.executable,'-c',{child!r}],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)",
        f"pathlib.Path({str(pid_path)!r}).write_text(str(child.pid))",
        f"ready=pathlib.Path({str(ready_path)!r})",
        f"for attempt in range({DRILL_READY_POLLS_MAX}):",
        "    if ready.exists(): break",
        f"    time.sleep({DRILL_POLL_SECONDS})",
        "if not ready.exists(): raise RuntimeError('background child did not become ready')",
        "print('parent completed')",
    ])
    pid = None
    try:
        result = command_case(runner, directory, "background-closed-pipes", parent, DRILL_COMMAND_SECONDS_MAX, None)
        pid = int(pid_path.read_text())
        if not ready_path.is_file():
            raise RuntimeError("background drill did not activate")
        for attempt in range(DRILL_READY_POLLS_MAX):
            if not running(pid):
                result.update(child_pid=pid, child_ready=True, child_running_after_cleanup=False)
                return result
            time.sleep(DRILL_POLL_SECONDS)
        raise RuntimeError("background child survived successful parent completion with closed pipes")
    finally:
        if pid is None and pid_path.is_file():
            pid = int(pid_path.read_text())
        if pid is not None and running(pid):
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


def main():
    parser = argparse.ArgumentParser(description="Allocated-only trusted child-process resource-policy drills; not an OS sandbox or untrusted-code isolation claim.")
    parser.add_argument("--allocated", action="store_true")
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if not args.allocated:
        parser.error("requires explicit root-allocated runtime; never run during source-only preparation")
    directory = args.out.resolve()
    directory.mkdir(parents=True, exist_ok=False)
    runner = load_runner()
    cases = [
        ("exact-output-limit", "import os; os.write(1,b'a'*32); os.write(2,b'b'*32)", DRILL_COMMAND_SECONDS_MAX, None),
        ("output-over-limit", "import os; os.write(1,b'x'*65)", DRILL_COMMAND_SECONDS_MAX, "bound(command_output_bytes_max=64)"),
        ("combined-output-over-limit", "import os; os.write(1,b'a'*33); os.write(2,b'b'*32)", DRILL_COMMAND_SECONDS_MAX, "bound(command_output_bytes_max=64)"),
        ("timeout-open-pipes", f"import time; time.sleep({DRILL_SLEEP_SECONDS})", DRILL_TIMEOUT_SECONDS_MAX, "bound(command_seconds_max=1)"),
        ("timeout-closed-pipes", f"import os,time; os.close(1); os.close(2); time.sleep({DRILL_SLEEP_SECONDS})", DRILL_TIMEOUT_SECONDS_MAX, "bound(command_seconds_max=1)"),
    ]
    results = [command_case(runner, directory, *case) for case in cases]
    results.append(background_case(runner, directory))
    evidence = {"status": "passed", "scope": "trusted test subprocess groups only; not OS sandboxing; detached-session descendants outside policy", "cases": results}
    (directory / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"passed runner resource-policy drills: {len(results)} cases")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError) as error:
        print(f"selfhost-func-runner-drill: {error}", file=sys.stderr)
        sys.exit(1)
