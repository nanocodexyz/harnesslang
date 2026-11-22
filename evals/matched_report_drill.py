import copy
import json
import math
import random
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
                if arm == args.baseline and attempt == 1:
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


def pin_fixture(args):
    for arm in (args.baseline, args.candidate):
        change_manifest(arm, lambda value: value.update(reporting_rules=report.reporting_rules.pin()))
        for directory in (arm / "rows").iterdir():
            path = directory / "result.json"
            row = report.read_json(path)
            row["reporting_rules"] = report.reporting_rules.pin()
            write(path, row)


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
    assert result["gates"]["paired_improvement_ci_positive"]
    assert result["paired"]["pass_difference"]["ci_95"][0] > 0
    assert math.isclose(result["candidate"]["cost_mean_usd"], 0.00165)
    for name in ("baseline", "candidate"):
        interval = result[name]["cost_mean_bootstrap_95"]
        assert interval["clusters"] == 3 and interval["attempts_per_cluster"] == 3
        assert interval["repetitions"] == 501 and interval["seed"] == 42
        assert math.isclose(interval["mean"], 0.00165)
        ratio = result[name]["cost_per_pass_bootstrap_95"]
        assert ratio["repetitions"] == 10000 and ratio["seed"] == 0
        expected_ratio = 0.00165 * 9 / result[name]["counts"]["passed"]
        assert math.isclose(result[name]["cost_per_pass"], expected_ratio)
        assert math.isclose(ratio["mean"], expected_ratio)
        assert ratio["zero_pass_replicates"] == 0
        assert result[name]["campaign_rules_status"] == "legacy_unpinned"
        assert all(math.isclose(value, 0.00165) for value in interval["ci_95"])
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
    assert "paired 95% CI lower bound > 0 pp" in completed.stdout


def check_unknown(result):
    fails(result)
    assert result["candidate"]["cost_mean_usd"] is None
    assert result["candidate"]["cost_total_usd"] is None
    assert result["candidate"]["cost_mean_bootstrap_95"] is None
    assert result["candidate"]["cost_per_pass"] is None
    assert result["candidate"]["cost_per_pass_bootstrap_95"] is None
    assert result["candidate"]["unknown_cost_rows"]
    if result["paired"]:
        assert result["paired"]["cost_difference_usd"] is None
    assert not result["gates"]["candidate_cost"]


def check_incomplete(result):
    fails(result)
    assert result["candidate"]["cost_mean_usd"] is None
    assert result["candidate"]["cost_mean_bootstrap_95"] is None
    assert result["candidate"]["cost_per_pass"] is None
    assert result["candidate"]["cost_per_pass_bootstrap_95"] is None


def uncertain_improvement(args):
    for directory in (args.baseline / "rows").iterdir():
        path = directory / "result.json"
        row = report.read_json(path)
        row["outcome"] = "failed" if (row["source"], row["task"]) == TASKS[0] else "passed"
        write(path, row)


def check_uncertain_improvement(result):
    fails(result)
    assert result["matched"]
    assert result["gates"]["quality_improvement"]
    assert result["paired"]["pass_difference"]["mean"] > 0.05
    assert result["paired"]["pass_difference"]["ci_95"][0] == 0
    assert not result["gates"]["paired_improvement_ci_positive"]
    assert all(passed for name, passed in result["gates"].items() if name != "paired_improvement_ci_positive")


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


def seal_ledger(directory):
    path = directory / "requests.jsonl"
    records = report.read_lines(path)
    for index, record in enumerate(records):
        key = "request-%d" % index
        record.update(request_id=key, meter_incarnation="fixture-incarnation")
        write(directory / "meter-intents" / (key + ".intent.json"),
              dict(request_id=key, incarnation="fixture-incarnation"))
        write(directory / "meter-intents" / (key + ".done.json"),
              dict(request_id=key, record=record, record_sha256=report.checkpoint_digest(record)))
    path.write_text("".join(json.dumps(record) + "\n" for record in records))
    write(directory / "meter-closed.json", dict(incarnation="fixture-incarnation"))


def ledger_mutation(args, kind):
    directory = first_row(args.candidate)
    seal_ledger(directory)
    root = directory / "meter-intents"
    if kind == "unresolved":
        write(root / "lost.intent.json", dict(request_id="lost", incarnation="previous-incarnation"))
    elif kind == "unclosed":
        (directory / "meter-closed.json").unlink()
    elif kind == "orphan-completion":
        (root / "request-0.intent.json").unlink()
    elif kind == "orphan-request":
        (root / "request-0.intent.json").unlink()
        (root / "request-0.done.json").unlink()
    elif kind == "duplicate":
        change_requests(args.candidate, lambda rows: rows.append(rows[0]))
        change_row(args.candidate, lambda row: row["meter"].update(requests=3))
    else:
        path = root / "request-0.done.json"
        value = report.read_json(path)
        if kind == "incarnation":
            value["record"]["meter_incarnation"] = "invalid"
            value["record_sha256"] = report.checkpoint_digest(value["record"])
        else:
            value[{"digest": "record_sha256", "identifier": "request_id"}[kind]] = "invalid"
        write(path, value)


def check_receipt_recovery(root):
    args = fixture(root)
    directory = first_row(args.candidate)
    seal_ledger(directory)
    before = report.build_report(args)
    assert before["status"] == "PASS"
    (directory / "requests.jsonl").unlink()
    after = report.build_report(args)
    assert after["status"] == "PASS", report.readable(after)
    assert after["candidate"]["cost_mean_bootstrap_95"] == before["candidate"]["cost_mean_bootstrap_95"]
    assert after["candidate"]["cost_total_usd"] == before["candidate"]["cost_total_usd"]
    write(directory / "meter-intents" / "lost.intent.json", dict(request_id="lost", incarnation="old"))
    check_unknown(report.build_report(args))


def check_cost_clusters(root):
    args = fixture(root)
    factors = (1, 2, 4)
    for arm in (args.baseline, args.candidate):
        for directory in (arm / "rows").iterdir():
            row = report.read_json(directory / "result.json")
            factor = factors[TASKS.index((row["source"], row["task"]))] * row["attempt"]
            path = directory / "requests.jsonl"
            records = report.read_lines(path)
            for record in records:
                record["usage"]["input_tokens"] *= factor
                record["usage"]["output_tokens"] *= factor
            path.write_text("".join(json.dumps(record) + "\n" for record in records))
    result = report.build_report(args)
    assert result["status"] == "PASS", report.readable(result)
    means = [0.00165 * factors[TASKS.index(task)] * 2 for task in sorted(TASKS)]
    randomizer = random.Random(args.seed)
    samples = sorted(sum(means[randomizer.randrange(3)] for _ in range(3)) / 3 for _ in range(args.bootstrap_reps))
    expected = [report.percentile(samples, probability) for probability in (0.025, 0.975)]
    for arm in ("baseline", "candidate"):
        interval = result[arm]["cost_mean_bootstrap_95"]
        assert math.isclose(interval["mean"], sum(means) / 3)
        assert all(math.isclose(first, second) for first, second in zip(interval["ci_95"], expected))
        assert interval["ci_95"][0] < interval["mean"] < interval["ci_95"][1]
    assert result["paired"]["cost_difference_usd"] == dict(mean=0.0, ci_95=[0.0, 0.0])
    change_requests(args.candidate, lambda rows: rows[0].update(status=0))
    check_unknown(report.build_report(args))
    assert report.build_report(args)["baseline"]["cost_mean_bootstrap_95"] is not None


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
        if "reporting_rules" in old:
            identity["reporting_rules"] = old["reporting_rules"]
        directories = sorted((arm / "rows").iterdir())
        identity["matrix"] = [directory.name for directory in directories]
        identity["images"] = {directory.name: "sha256:" + "9" * 64 for directory in directories}
        write(arm / "campaign.json", dict(schema=1, identity=identity, created=123))
        for directory in directories:
            seal_ledger(directory)
            path = directory / "result.json"
            row = report.read_json(path)
            row.pop("campaign_sha256")
            write(path, row)
            write(directory / "checkpoint.json", dict(schema=1, stage="completed",
                  identity=dict(campaign=report.checkpoint_digest(identity), row=row["id"]),
                  result_sha256=report.checkpoint_digest(row)))


def change_checkpoint_identity(arm, action):
    wrapper = report.read_json(arm / "campaign.json")
    action(wrapper["identity"])
    write(arm / "campaign.json", wrapper)
    for directory in (arm / "rows").iterdir():
        path = directory / "checkpoint.json"
        state = report.read_json(path)
        state["identity"]["campaign"] = report.checkpoint_digest(wrapper["identity"])
        write(path, state)


def check_policy_treatment(root):
    args = fixture(root)
    checkpoint_fixture(args)
    paths = ["examples/codex_agent.hl", *report.POLICY_FIXTURE_PATHS]
    for arm in (args.baseline, args.candidate):
        change_checkpoint_identity(arm, lambda identity: identity["source_files"].update(
            {path: ("a" if arm == args.baseline else "b") * 64 for path in paths}))
    args.allow_source_change = ["src/agent.rs", *paths]
    result = report.build_report(args)
    assert result["status"] == "PASS", report.readable(result)
    assert set(result["treatment"]["observed_changes"]["source"]) == set(args.allow_source_change)
    for path in paths:
        args.allow_source_change.remove(path)
        fails(report.build_report(args))
        args.allow_source_change.append(path)
    blocked = ("scripts/program-literals.py", "scripts/fixtures/prompts/tools.json",
               "scripts/fixtures/prompts/codex-texts.json.extra", "scripts/check.sh",
               "evals/run.py", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml")
    for path in blocked:
        assert report.measurement_path(path)
        original = report.read_json(args.candidate / "campaign.json")["identity"]["source_files"].get(path)
        change_checkpoint_identity(args.candidate, lambda identity: identity["source_files"].update({path: "e" * 64}))
        args.allow_source_change.append(path)
        fails(report.build_report(args))
        args.allow_source_change.remove(path)
        if original is None:
            change_checkpoint_identity(args.candidate, lambda identity: identity["source_files"].pop(path))
        else:
            change_checkpoint_identity(args.candidate, lambda identity: identity["source_files"].update({path: original}))
    assert report.build_report(args)["status"] == "PASS"
    command = [sys.executable, str(ROOT / "report.py"), "--baseline", str(args.baseline),
               "--candidate", str(args.candidate), "--suite", str(args.suite), "--expected-tasks", "3"]
    for path in args.allow_source_change:
        command.extend(["--allow-source-change", path])
    completed = subprocess.run(command, capture_output=True, text=True)
    assert completed.returncode == 0, completed.stderr
    completed = subprocess.run(command + ["--allow-source-change", "scripts/check.sh"], capture_output=True, text=True)
    assert completed.returncode == 2 and "cannot be treatment exceptions" in completed.stderr


def check_checkpoint(root):
    args = fixture(root)
    checkpoint_fixture(args)
    fails(report.build_report(args))
    args.allow_source_change = ["src/agent.rs"]
    result = report.build_report(args)
    assert result["status"] == "PASS", report.readable(result)
    assert result["treatment"]["observed_changes"]["source"] == ["src/agent.rs"]
    change_checkpoint_identity(args.candidate, lambda identity: identity["agent_tree"].update({"bin/hl-agent": "e" * 64}))
    fails(report.build_report(args))
    args.allow_agent_change = ["bin/hl-agent"]
    assert report.build_report(args)["status"] == "PASS"
    change_checkpoint_identity(args.candidate, lambda identity: identity["source_files"].update({"Cargo.lock": "f" * 64}))
    args.allow_source_change.append("Cargo.lock")
    fails(report.build_report(args))
    args.allow_source_change.remove("Cargo.lock")
    change_checkpoint_identity(args.candidate, lambda identity: identity["source_files"].pop("Cargo.lock"))
    assert report.build_report(args)["status"] == "PASS"
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


def closed_timeout(args):
    directory = first_row(args.candidate)
    seal_ledger(directory)
    records = report.read_lines(directory / "requests.jsonl")
    for record in records:
        key = record["request_id"]
        intent = dict(schema=3, request_id=key, incarnation="fixture-incarnation", provider="openai",
            path=record["path"], model=record["request"]["model"], started=1.0, body_sha256="1" * 64,
            attempt_identity_sha256=report.checkpoint_digest({"run": directory.name}))
        record.update(provider="openai", started=1.0, usage_known=True, intent_sha256=report.checkpoint_digest(intent),
            terminal=dict(complete=True, identity_matches=True, model=intent["model"], response_sha256="2" * 64,
                provider_response_id="fixture-response-" + key, usage_sha256=report.checkpoint_digest(record["usage"])))
        write(directory / "meter-intents" / (key + ".intent.json"), intent)
        write(directory / "meter-intents" / (key + ".done.json"),
              dict(request_id=key, record=record, record_sha256=report.checkpoint_digest(record)))
    (directory / "requests.jsonl").write_text("".join(json.dumps(record) + "\n" for record in records))
    status = "observed_after_timeout_reconciliation"
    change_row(args.candidate, lambda row: row.update(outcome="failed", agent_result={"timed_out": True},
        reporting_rules=report.reporting_rules.pin(), accounting_status=status, meter=dict(requests=2, ok_requests=2, accounting_status=status,
            unknown_requests=0, unproven_intents=0, durable_ledger=dict(closed=True, intents=2, completed=2, unknown=0, unmatched_records=0))))


def check_timeout_accounting(root):
    args = fixture(root)
    pin_fixture(args)
    closed_timeout(args)
    result = report.build_report(args)
    assert math.isclose(result["candidate"]["cost_per_pass"], 0.00165 * 9 / 8)
    assert result["candidate"]["cost_total_usd"] is not None
    assert result["candidate"]["counts"]["failed"] == 1
    for field, bad in (("closed", False), ("intents", 3), ("completed", 1), ("unknown", 1), ("unmatched_records", 1),
                       ("completed", True), ("unknown", False), ("unmatched_records", False), ("active", False)):
        change_row(args.candidate, lambda row: row["meter"]["durable_ledger"].update({field: bad}))
        check_unknown(report.build_report(args))
        closed_timeout(args)
    for field, bad in (("unproven_intents", 1), ("unproven_intents", False), ("unknown_requests", False)):
        change_row(args.candidate, lambda row: row["meter"].update({field: bad}))
        check_unknown(report.build_report(args))
        closed_timeout(args)
    change_requests(args.candidate, lambda rows: [record.pop("intent_sha256") for record in rows])
    seal_ledger(first_row(args.candidate))
    result = report.build_report(args)
    assert result["candidate"]["rows"][0]["pricing"]["ledger"]["unproven_intents"] == 2
    check_unknown(result)
    closed_timeout(args)
    change_row(args.candidate, lambda row: row["meter"].pop("durable_ledger"))
    check_unknown(report.build_report(args))
    closed_timeout(args)
    change_row(args.candidate, lambda row: row.update(accounting_status="unknown_at_timeout"))
    check_unknown(report.build_report(args))
    closed_timeout(args)
    (first_row(args.candidate) / "meter-closed.json").unlink()
    check_unknown(report.build_report(args))
    closed_timeout(args)
    change_manifest(args.candidate, lambda value: value.pop("reporting_rules"))
    check_unknown(report.build_report(args))
    pin_fixture(args)
    change_row(args.candidate, lambda row: row.pop("reporting_rules"))
    check_unknown(report.build_report(args))
    closed_timeout(args)
    change_row(args.candidate, lambda row: row["reporting_rules"].update(sha256="0" * 64))
    check_unknown(report.build_report(args))
    closed_timeout(args)
    change_row(args.candidate, lambda row: row.update(outcome="passed"))
    result = report.build_report(args)
    fails(result)
    assert any("timed-out attempt cannot pass" in issue for issue in result["candidate"]["issues"])


def check_ratio_clusters(root):
    args = fixture(root)
    tasks = sorted(TASKS)
    for directory in (args.candidate / "rows").iterdir():
        path = directory / "result.json"
        row = report.read_json(path)
        index = tasks.index((row["source"], row["task"]))
        row["outcome"] = "passed" if index == 0 or (index == 1 and row["attempt"] == 1) else "failed"
        write(path, row)
        records = report.read_lines(directory / "requests.jsonl")
        for record in records:
            for field in ("input_tokens", "output_tokens"):
                record["usage"][field] *= (index + 1) * row["attempt"]
        (directory / "requests.jsonl").write_text("".join(json.dumps(record) + "\n" for record in records))
    result = report.build_report(args)
    summary = result["candidate"]
    clusters = [(0.00165 * 6 * (index + 1), passed) for index, passed in enumerate((3, 1, 0))]
    assert math.isclose(summary["cost_per_pass"], sum(cost for cost, passed in clusters) / 4)
    randomizer = random.Random(0)
    samples = []
    for _ in range(10000):
        selected = [clusters[randomizer.randrange(3)] for _ in range(3)]
        passes = sum(passed for cost, passed in selected)
        samples.append(sum(cost for cost, passed in selected) / passes if passes else math.inf)
    samples.sort()
    interval = summary["cost_per_pass_bootstrap_95"]
    assert interval["clusters"] == 3 and interval["attempts_per_cluster"] == 3
    assert interval["zero_pass_replicates"] == samples.count(math.inf) > 0
    assert interval["ci_95"][1] is None
    assert math.isclose(interval["ci_95"][0], report.percentile(samples, 0.025))
    finite_only = [value for value in samples if math.isfinite(value)]
    assert math.isfinite(report.percentile(finite_only, 0.975))
    assert result == report.build_report(args)
    json.dumps(result, allow_nan=False)
    for directory in (args.candidate / "rows").iterdir():
        path = directory / "result.json"
        row = report.read_json(path)
        row["outcome"] = "failed"
        write(path, row)
    summary = report.build_report(args)["candidate"]
    assert summary["cost_total_usd"] > 0 and summary["cost_mean_usd"] > 0
    assert summary["cost_per_pass"] is None and summary["cost_per_pass_bootstrap_95"] is None


def check_rules(root):
    args = fixture(root)
    original = {str(path): path.read_bytes() for path in root.rglob("*") if path.is_file()}
    result = report.build_report(args)
    assert result["reporting_rules"]["pin"] == report.reporting_rules.pin()
    assert all(Path(path).read_bytes() == content for path, content in original.items())
    rules = report.reporting_rules.manifest()
    assert set(rules["implementation_sha256"]) == {"report.py", "reporting_rules.py", "checkpoint_meter.py", "checkpoint.py", "run.py", "checkpoint_phase.py", "provenance.py"}
    assert all(report.valid_digest(value) for value in rules["implementation_sha256"].values())
    assert report.checkpoint_digest(rules) == report.reporting_rules.pin()["sha256"]
    rules["prices_per_million_tokens"]["gpt-6-luna"]["input"] *= 2
    assert report.checkpoint_digest(rules) != report.reporting_rules.pin()["sha256"]
    assert report.reporting_rules.manifest() != rules
    pin_fixture(args)
    result = report.build_report(args)
    assert result["status"] == "PASS", report.readable(result)
    assert all(result[name]["campaign_rules_status"] == "pinned" for name in ("baseline", "candidate"))
    for field, bad in (("sha256", "0" * 64), ("version", "foreign-version")):
        change_manifest(args.candidate, lambda value: value["reporting_rules"].update({field: bad}))
        result = report.build_report(args)
        fails(result)
        assert result["candidate"]["campaign_rules_status"] == "pin_mismatch"
        assert any("reporting rules pin" in issue for issue in result["candidate"]["issues"])
        change_manifest(args.candidate, lambda value: value.update(reporting_rules=report.reporting_rules.pin()))
    checkpoint_fixture(args)
    for arm in (args.baseline, args.candidate):
        change_checkpoint_identity(arm, lambda value: value.update(reporting_rules=report.reporting_rules.pin()))
    args.allow_source_change = ["src/agent.rs"]
    assert report.build_report(args)["status"] == "PASS"
    change_checkpoint_identity(args.candidate, lambda value: value["reporting_rules"].update(sha256="0" * 64))
    fails(report.build_report(args))


def check_single_arm(root):
    args = fixture(root)
    command = [sys.executable, str(ROOT / "report.py"), "--single-arm", str(args.candidate),
               "--suite", str(args.suite), "--expected-tasks", "3", "--attempts", "3"]
    completed = subprocess.run(command, capture_output=True, text=True)
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout)
    assert result["status"] == "COMPLETE" and not result["matched"] and result["paired"] is None
    assert "NOT_MATCHED" in result["profile"] and "gates" not in result
    assert result["arm"]["pass_wilson_95"] == report.wilson(9, 9)
    assert math.isclose(result["arm"]["cost_per_pass"], 0.00165)
    completed = subprocess.run(command + ["--summary"], capture_output=True, text=True)
    assert completed.returncode == 0 and "no matched comparison" in completed.stdout
    assert "10000" not in completed.stderr
    completed = subprocess.run(command + ["--baseline", str(args.baseline)], capture_output=True, text=True)
    assert completed.returncode == 2
    completed = subprocess.run(command + ["--allow-source-change", "src/agent.rs"], capture_output=True, text=True)
    assert completed.returncode == 2
    shutil.rmtree(first_row(args.candidate))
    completed = subprocess.run(command, capture_output=True, text=True)
    assert completed.returncode == 1, completed.stderr
    result = json.loads(completed.stdout)
    assert result["status"] == "INCOMPLETE"
    assert result["arm"]["cost_per_pass"] is None
    assert result["arm"]["cost_per_pass_bootstrap_95"] is None
    assert result["arm"]["pass_wilson_95"] is None


def main():
    with tempfile.TemporaryDirectory(prefix="matched-report-") as temporary:
        root = Path(temporary)
        valid = root / "valid"
        valid.mkdir()
        check_valid(fixture(valid))
        mutations = [
            ("incomplete", lambda args: shutil.rmtree(first_row(args.candidate)), check_incomplete),
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
            ("uncertain-improvement", uncertain_improvement, check_uncertain_improvement),
            ("no-improvement", lambda args: setattr(args, "min_improvement_pp", 34), fails),
            ("over-budget", lambda args: setattr(args, "max_cost", 0.001), fails),
        ]
        mutations.extend(("ledger-" + kind, lambda args, kind=kind: ledger_mutation(args, kind), check_unknown)
                         for kind in ("unresolved", "unclosed", "orphan-completion", "orphan-request", "duplicate", "digest", "incarnation", "identifier"))
        rejected = [run_mutant(root, *mutation) for mutation in mutations]
        for name, check in (("timeout-accounting", check_timeout_accounting), ("ratio-clusters", check_ratio_clusters), ("rules", check_rules), ("single-arm", check_single_arm)):
            directory = root / name
            directory.mkdir()
            check(directory)
        varied = root / "varied-cost"
        varied.mkdir()
        check_cost_clusters(varied)
        recovery = root / "receipt-recovery"
        recovery.mkdir()
        check_receipt_recovery(recovery)
        checkpoint = root / "checkpoint"
        checkpoint.mkdir()
        check_checkpoint(checkpoint)
        policy = root / "policy-treatment"
        policy.mkdir()
        check_policy_treatment(policy)
        evidence = valid / "journal-counters.json"
        write(evidence, {"streams": [dict(row="before-1", aborted_cost_unknown=True),
                                     dict(row="before-2", aborted_cost_unknown=True)]})
        args = fixture(valid)
        args.baseline_accounting = evidence
        result = report.build_report(args)
        assert result["baseline"]["cost_total_usd"] is None
        assert result["baseline"]["cost_mean_usd"] is None
        assert result["baseline"]["cost_mean_bootstrap_95"] is None
        assert len(result["baseline"]["unknown_external_streams"]) == 2
        assert result["paired"]["cost_difference_usd"] is None
        assert result["status"] == "FAIL"
        drops = root / "dropped-baseline"
        drops.mkdir()
        args = fixture(drops)
        for directory in sorted((args.baseline / "rows").iterdir())[:2]:
            path = directory / "requests.jsonl"
            records = report.read_lines(path)
            records[0]["status"] = 0
            path.write_text("".join(json.dumps(record) + "\n" for record in records))
        result = report.build_report(args)
        assert len(result["baseline"]["unknown_cost_rows"]) == 2
        assert result["baseline"]["cost_total_usd"] is None
        assert result["baseline"]["cost_mean_usd"] is None
        assert result["baseline"]["cost_mean_bootstrap_95"] is None
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
        for name in ("baseline", "candidate"):
            assert report.build_report(args)[name]["cost_mean_bootstrap_95"]["clusters"] == 60
            assert report.build_report(args)[name]["cost_mean_bootstrap_95"]["attempts_per_cluster"] == 3
        historical = root / "historical"
        historical.mkdir()
        args = fixture(historical, suite60[:35])
        assert report.build_report(args)["profile"] == "CUSTOM_35_TASKS_105_ATTEMPTS_NOT_MATCHED180"
        print(json.dumps(dict(result="passed", clusters=3, attempts_per_arm=9, mutants_rejected=rejected,
                              full_matrix="60 tasks x 3 attempts x 2 arms", reporting_rules=report.reporting_rules.pin(),
                              reporting_checks=["ratio-all-attempts", "task-correlation", "zero-pass-replicates", "zero-passes",
                                                "version-and-price-hash", "historical-no-write", "closed-timeout-proof-mutations", "single-arm-cli", "incomplete-no-CI"],
                              model_calls=0, network_calls=0)))


if __name__ == "__main__":
    main()
