import copy
import json
import math
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace

import report

TASKS = [("nanoeval", "shared"), ("terminal-bench-2.1", "shared"), ("terminal-bench-2.1", "other")]
ATTEMPTS = 3
ROOT = Path(__file__).resolve().parent


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True, indent=1) + "\n")


def fixture(root, tasks=TASKS):
    suite = root / "suite.txt"
    suite.write_text("".join(source + " " + task + "\n" for source, task in tasks))
    args = SimpleNamespace(baseline=root / "baseline", candidate=root / "candidate", suite=suite,
                           attempts=ATTEMPTS, expected_tasks=len(tasks), agent="hl-agent", min_success=0.92,
                           max_cost=0.010, min_improvement_pp=5.0, bootstrap_reps=501, seed=42)
    for arm in (args.baseline, args.candidate):
        manifest = {"version": 1, "source": {"commit": "a" * 40 if arm == args.baseline else "b" * 40,
                                              "tracked_sha256": "c" * 64},
                    "agent_root_sha256": "d" * 64, "campaign_suite_sha256": "e" * 64,
                    "transport_sha256": "f" * 64, "runtime": {"machine": "mock", "python": [3, 12, 0]},
                    "config": dict(agents=["hl-agent"], attempts=ATTEMPTS, model="gpt-6-luna", parallel=2,
                                   limits="none", agent_memory_mb=0, max_turns=None, openai_route="chatgpt",
                                   hl_agent_arg=["--effort", "medium"], ca_sha256=None),
                    "tasks": [dict(source=source, task=task, package_sha256="1" * 64, tree_sha256="2" * 64)
                              for source, task in sorted(tasks)]}
        write(arm / "campaign.json", manifest)
        identity = report.manifest_identity(manifest)
        for index, (source, task) in enumerate(tasks):
            for attempt in range(1, ATTEMPTS + 1):
                identifier = "%s.%s.hl-agent.%d" % (source, task, attempt)
                directory = arm / "rows" / identifier
                row = dict(id=identifier, source=source, task=task, attempt=attempt, agent="hl-agent",
                           model="gpt-6-luna", outcome="passed", campaign_sha256=identity,
                           image="sha256:" + "9" * 64, meter=dict(requests=2, ok_requests=2))
                if arm == args.baseline and index == 0:
                    row["outcome"] = "failed"
                write(directory / "result.json", row)
                records = [dict(run=identifier, path="/v1/responses", status=200, request=dict(model=model),
                                usage=dict(input_tokens=1000, output_tokens=100, cache_read_tokens=0,
                                           cache_write_tokens=0, cache_write_1h_tokens=0))
                           for model in ("gpt-6-luna", "gpt-6.1-terra")]
                (directory / "requests.jsonl").write_text("".join(json.dumps(record) + "\n" for record in records))
    return args


def first_row(arm):
    return sorted((arm / "rows").iterdir())[0]


def change_row(arm, action):
    path = first_row(arm) / "result.json"
    row = report.read_json(path)
    action(row)
    write(path, row)


def change_manifest(arm, action):
    path = arm / "campaign.json"
    manifest = report.read_json(path)
    action(manifest)
    write(path, manifest)
    for directory in (arm / "rows").iterdir():
        row_path = directory / "result.json"
        row = report.read_json(row_path)
        row["campaign_sha256"] = report.manifest_identity(manifest)
        write(row_path, row)


def change_requests(arm, action):
    path = first_row(arm) / "requests.jsonl"
    rows = report.read_lines(path)
    action(rows)
    path.write_text("".join(json.dumps(row) + "\n" for row in rows))


def run_mutant(root, name, action, verify):
    mutant = root / name
    mutant.mkdir()
    args = fixture(mutant)
    action(args)
    result = report.build_report(args)
    verify(result)
    return name


def fails(result):
    assert result["status"] == "FAIL", result


def check_valid(args):
    result = report.build_report(args)
    assert result["status"] == "PASS", report.readable(result)
    assert result["profile"].endswith("NOT_MATCHED180")
    assert result["matched"]
    assert result["baseline"]["counts"] == dict(passed=6, failed=3, errored=0, invalid=0)
    assert result["paired"]["clusters"] == 3
    assert result["paired"]["seed"] == 42
    assert result["paired"]["repetitions"] == 501
    assert math.isclose(result["paired"]["pass_difference"]["mean"], 1 / 3)
    assert math.isclose(result["candidate"]["cost_mean_usd"], 0.00165)
    assert result == report.build_report(args)
    assert report.wilson(2, 3)[0] > 0.20
    encoded = json.dumps(result, allow_nan=False)
    assert 'NaN' not in encoded
    command = [sys.executable, str(ROOT / "report.py"), "--baseline", str(args.baseline),
               "--candidate", str(args.candidate), "--suite", str(args.suite), "--expected-tasks", "3",
               "--bootstrap-reps", "501", "--seed", "42"]
    completed = subprocess.run(command, capture_output=True, text=True)
    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["paired"] == result["paired"]
    completed = subprocess.run(command + ["--summary"], capture_output=True, text=True)
    assert completed.returncode == 0
    assert "95% Wilson" in completed.stdout and "3 task clusters" in completed.stdout


def check_unknown(result):
    fails(result)
    assert result["candidate"]["cost_mean_usd"] is None
    assert result["candidate"]["cost_total_usd"] is None
    assert result["candidate"]["unknown_cost_rows"]
    if result["paired"]:
        assert result["paired"]["cost_difference_usd"] is None
    assert not result["gates"]["candidate_cost"]


def check_error(result):
    fails(result)
    assert result["matched"]
    assert result["candidate"]["counts"]["errored"] == 1
    assert result["candidate"]["expected_attempts"] == 9
    assert math.isclose(result["candidate"]["pass_rate"], 8 / 9)
    assert result["candidate"]["pass_wilson_95"] == report.wilson(8, 9)
    assert result["paired"]["clusters"] == 3


def duplicate(args):
    directory = first_row(args.candidate)
    shutil.copytree(directory, directory.with_name(directory.name + ".duplicate"))


def raw_absent(args):
    directory = first_row(args.candidate)
    (directory / "responses.jsonl").write_text('{}\n{}\n')


def response_mismatch(args, field):
    directory = first_row(args.candidate)
    messages = [dict(model=model, usage=dict(input_tokens=1000, output_tokens=100,
                                           input_tokens_details=dict(cached_tokens=0)))
                for model in ("gpt-6-luna", "gpt-6.1-terra")]
    if field == "model":
        messages[0]["model"] = "gpt-6.1-terra"
    else:
        messages[0]["usage"]["input_tokens"] = 999
    (directory / "responses.jsonl").write_text("".join(json.dumps(message) + "\n" for message in messages))


def checkpoint_fixture(args):
    for arm in (args.baseline, args.candidate):
        old = report.read_json(arm / "campaign.json")
        parameters = dict(old["config"])
        ca = parameters.pop("ca_sha256")
        parameters["agents"] = ",".join(parameters["agents"])
        parameters["only"] = None
        identity = dict(source_commit=old["source"]["commit"],
                        source_files={"src/agent.rs": "a" * 64 if arm == args.baseline else "b" * 64,
                                      "evals/run.py": "c" * 64},
                        parameters=parameters, suite_sha256=old["campaign_suite_sha256"],
                        agent_tree={"bin/hl-agent": "d" * 64},
                        tasks={task["source"] + "/" + task["task"]: task["package_sha256"] for task in old["tasks"]},
                        task_trees={task["source"] + "/" + task["task"]: task["tree_sha256"] for task in old["tasks"]},
                        configs_sha256="f" * 64, ca_sha256=ca, runtime=old["runtime"],
                        transport_sha256=old["transport_sha256"], cache=dict(enabled=False))
        directories = sorted((arm / "rows").iterdir())
        identity["matrix"] = [directory.name for directory in directories]
        identity["images"] = {directory.name: "sha256:" + "9" * 64 for directory in directories}
        write(arm / "campaign.json", dict(schema=1, identity=identity, created=123))
        for directory in directories:
            path = directory / "result.json"
            row = report.read_json(path)
            row.pop("campaign_sha256")
            write(path, row)
            write(directory / "checkpoint.json", dict(schema=1, stage="completed",
                  identity=dict(campaign=report.checkpoint_digest(identity), row=row["id"]),
                  result_sha256=report.checkpoint_digest(row)))


def check_checkpoint(root):
    args = fixture(root)
    checkpoint_fixture(args)
    result = report.build_report(args)
    assert result["status"] == "PASS", report.readable(result)
    directory = first_row(args.candidate)
    path = directory / "checkpoint.json"
    state = report.read_json(path)
    state["result_sha256"] = "0" * 64
    write(path, state)
    fails(report.build_report(args))
    state["result_sha256"] = report.checkpoint_digest(report.read_json(directory / "result.json"))
    write(path, state)
    result_path = directory / "result.json"
    result_path.unlink()
    write(directory / "infrastructure.json", dict(id=directory.name, outcome="errored", infrastructure_status="blocked"))
    result = report.build_report(args)
    fails(result)
    assert result["candidate"]["counts"]["errored"] == 1
    assert result["candidate"]["observed_rows"] == 9
    assert result["candidate"]["pass_rate"] == 8 / 9
    assert not result["candidate"]["complete"]


def main():
    with tempfile.TemporaryDirectory(prefix="matched-report-") as temporary:
        root = Path(temporary)
        valid = root / "valid"
        valid.mkdir()
        check_valid(fixture(valid))
        mutations = [
            ("incomplete", lambda args: shutil.rmtree(first_row(args.candidate)), fails),
            ("duplicate", duplicate, fails),
            ("unknown-task", lambda args: change_row(args.candidate, lambda row: row.update(task="foreign")), fails),
            ("unknown-agent", lambda args: change_row(args.candidate, lambda row: row.update(agent="nanocode")), fails),
            ("unknown-outcome", lambda args: change_row(args.candidate, lambda row: row.update(outcome="skipped")), fails),
            ("config", lambda args: change_manifest(args.candidate, lambda value: value["config"].update(limits="task")), fails),
            ("source-pin", lambda args: change_manifest(args.baseline, lambda value: value.pop("source")), fails),
            ("task-pin", lambda args: change_manifest(args.candidate, lambda value: value["tasks"][0].update(tree_sha256="3" * 64)), fails),
            ("image", lambda args: change_row(args.candidate, lambda row: row.update(image="other")), fails),
            ("cache", lambda args: change_row(args.candidate, lambda row: row.update(verifier_cache_sha256="4" * 64)), fails),
            ("provenance", lambda args: change_row(args.candidate, lambda row: row.update(campaign_sha256="5" * 64)), fails),
            ("infra-error", lambda args: change_row(args.candidate, lambda row: row.update(outcome="errored")), check_error),
            ("non200", lambda args: change_requests(args.candidate, lambda rows: rows[0].update(status=429)), check_unknown),
            ("missing-status", lambda args: change_requests(args.candidate, lambda rows: rows[0].pop("status")), check_unknown),
            ("missing-usage", lambda args: change_requests(args.candidate, lambda rows: rows[0].pop("usage")), check_unknown),
            ("raw-missing-usage", raw_absent, check_unknown),
            ("response-model", lambda args: response_mismatch(args, "model"), check_unknown),
            ("response-usage", lambda args: response_mismatch(args, "usage"), check_unknown),
            ("unknown-model", lambda args: change_requests(args.candidate, lambda rows: rows[0]["request"].update(model="unknown")), check_unknown),
            ("accounting-lost", lambda args: change_row(args.candidate, lambda row: row.update(accounting_status="unknown_after_infrastructure")), check_unknown),
            ("unrecognized-endpoint", lambda args: change_requests(args.candidate, lambda rows: rows[0].update(path="/billable/unknown")), check_unknown),
            ("negative-usage", lambda args: change_requests(args.candidate, lambda rows: rows[0]["usage"].update(input_tokens=-1)), check_unknown),
            ("missing-log", lambda args: (first_row(args.candidate) / "requests.jsonl").unlink(), check_unknown),
            ("missing-request", lambda args: change_requests(args.candidate, lambda rows: rows.pop()), check_unknown),
            ("no-improvement", lambda args: setattr(args, "min_improvement_pp", 34), fails),
            ("over-budget", lambda args: setattr(args, "max_cost", 0.001), fails),
        ]
        rejected = [run_mutant(root, *mutation) for mutation in mutations]
        checkpoint = root / "checkpoint"
        checkpoint.mkdir()
        check_checkpoint(checkpoint)
        evidence = valid / "journal-counters.json"
        write(evidence, {"streams": [dict(row="before-1", aborted_cost_unknown=True),
                                     dict(row="before-2", aborted_cost_unknown=True)]})
        args = fixture(valid)
        args.baseline_accounting = evidence
        result = report.build_report(args)
        assert result["baseline"]["cost_total_usd"] is None
        assert result["baseline"]["cost_mean_usd"] is None
        assert len(result["baseline"]["unknown_external_streams"]) == 2
        assert result["paired"]["cost_difference_usd"] is None
        assert result["status"] == "FAIL"
        suite60 = report.read_suite(ROOT / "suite-60.txt")
        assert len(suite60) == 60
        assert sum(source == "nanoeval" for source, task in suite60) == 3
        assert sum(source == "terminal-bench-2.1" for source, task in suite60) == 57
        full = root / "full"
        full.mkdir()
        args = fixture(full, suite60)
        assert report.build_report(args)["profile"] == "MATCHED180"
        assert report.build_report(args)["paired"]["clusters"] == 60
        historical = root / "historical"
        historical.mkdir()
        args = fixture(historical, suite60[:35])
        assert report.build_report(args)["profile"] == "CUSTOM_35_TASKS_105_ATTEMPTS_NOT_MATCHED180"
        print(json.dumps(dict(result="passed", clusters=3, attempts_per_arm=9, mutants_rejected=rejected,
                              full_matrix="60 tasks x 3 attempts x 2 arms", model_calls=0, network_calls=0)))


if __name__ == "__main__":
    main()
