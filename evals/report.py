import argparse
import hashlib
import json
import math
from pathlib import Path
import random
import sys

import checkpoint_meter
import reporting_rules

WILSON_Z_95 = 1.959964
PRICES_PER_MTOK = reporting_rules.PRICES_PER_MTOK
PRICES_SOURCE = reporting_rules.PRICES_SOURCE
TOKENS_PER_MTOK = 1_000_000
MODEL_ENDPOINTS = ("/messages", "/responses", "/responses/compact", "/chat/completions")
TOKEN_FIELDS = ("input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens")
OUTCOMES = ("passed", "failed", "errored")
BINDING_FIELDS = ("meter_port", "relay_port", "meter_host")
POLICY_FIXTURE_PATHS = ("scripts/fixtures/prompts/codex-instructions.md", "scripts/fixtures/prompts/codex-texts.json")
REQUIRED_CONFIG = ("agents", "attempts", "model", "parallel", "limits", "agent_memory_mb", "max_turns", "openai_route", "hl_agent_arg", "ca_sha256")


def wilson(passed, total):
    if not total:
        return None
    probability = passed / total
    denominator = 1 + WILSON_Z_95 ** 2 / total
    centre = (probability + WILSON_Z_95 ** 2 / (2 * total)) / denominator
    half = WILSON_Z_95 * math.sqrt(probability * (1 - probability) / total + WILSON_Z_95 ** 2 / (4 * total * total)) / denominator
    return [centre - half, centre + half]


def token_count(value):
    return type(value) is int and value >= 0


def cost(model, usage):
    if model not in PRICES_PER_MTOK:
        raise ValueError("unknown request model: " + str(model))
    if not isinstance(usage, dict) or any(not token_count(usage.get(key)) for key in TOKEN_FIELDS):
        raise ValueError("missing or invalid usage")
    hour = usage.get("cache_write_1h_tokens", 0)
    if not token_count(hour) or hour > usage["cache_write_tokens"]:
        raise ValueError("invalid cache write split")
    price = PRICES_PER_MTOK[model]
    return (usage["input_tokens"] * price["input"] + usage["output_tokens"] * price["output"]
            + (usage["cache_write_tokens"] - hour) * price["cache_write_5m"]
            + hour * price["cache_write_1h"] + usage["cache_read_tokens"] * price["cache_read"]) / TOKENS_PER_MTOK


def read_json(path):
    with path.open() as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError(str(path) + ": expected object")
    return value


def read_lines(path):
    with path.open() as handle:
        return [json.loads(line) for line in handle]


def read_suite(path):
    tasks = []
    for number, line in enumerate(path.read_text().splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) != 2:
            raise ValueError("suite line %d must be source task" % number)
        tasks.append(tuple(parts))
    if not tasks or len(tasks) != len(set(tasks)):
        raise ValueError("empty or duplicate suite")
    return sorted(tasks)


def raw_usage(message):
    usage = message.get("usage")
    if not isinstance(usage, dict) or any(not token_count(usage.get(key)) for key in ("input_tokens", "output_tokens")):
        raise ValueError("response has absent/invalid usage")
    cached = (usage.get("input_tokens_details") or {}).get("cached_tokens", 0)
    anthropic = "cache_read_input_tokens" in usage or "cache_creation_input_tokens" in usage
    return {"input_tokens": usage["input_tokens"] if anthropic else usage["input_tokens"] - cached,
            "output_tokens": usage["output_tokens"],
            "cache_read_tokens": usage.get("cache_read_input_tokens", 0) if anthropic else cached,
            "cache_write_tokens": usage.get("cache_creation_input_tokens", 0),
            "cache_write_1h_tokens": (usage.get("cache_creation") or {}).get("ephemeral_1h_input_tokens", 0)}


def request_cost(record, response, row_id, intent=None):
    if not isinstance(record, dict):
        raise ValueError("invalid request record")
    if "not_dispatched" in record:
        if record.get("run") != row_id or not checkpoint_meter.not_dispatched(intent, record):
            raise ValueError("invalid or unbound NotDispatched record")
        return 0.0, None
    if record.get("usage_known") is False:
        raise ValueError("request usage not established")
    if record.get("status") != 200 or record.get("error"):
        raise ValueError("request status/error: " + str(record.get("status")))
    if record.get("run", row_id) != row_id:
        raise ValueError("request run differs from row")
    model = (record.get("request") or {}).get("model")
    usage = record.get("usage")
    amount = cost(model, usage)
    if response is None and not any(usage.get(field, 0) for field in TOKEN_FIELDS):
        raise ValueError("zero normalized usage without response evidence")
    if response is not None and response.get("model", model) != model:
        raise ValueError("request/response model mismatch")
    if response is not None:
        observed = raw_usage(response)
        if any(observed[key] != usage.get(key, 0) for key in observed):
            raise ValueError("request/response usage mismatch")
    return amount, model


def ledger_json(path, issues):
    try:
        return read_json(path)
    except (OSError, ValueError) as error:
        issues.append("invalid durable meter evidence %s: %s" % (path.name, error))
        return {}


def proven_terminal(intent, record, directory):
    if type(intent.get("schema")) is not int or intent["schema"] not in (2, 3):
        return False
    if "not_dispatched" in record:
        return checkpoint_meter.bound_record(intent, record, directory)
    terminal = record.get("terminal") or {}
    return (record.get("usage_known") is True and terminal.get("complete") is True
        and terminal.get("identity_matches") is True and valid_digest(terminal.get("response_sha256"))
        and isinstance(terminal.get("provider_response_id"), str) and bool(terminal["provider_response_id"])
        and terminal.get("model") == intent.get("model") and bool(terminal.get("model"))
        and checkpoint_meter.bound_record(intent, record, directory))


def meter_ledger(directory, records, required):
    root = directory / "meter-intents"
    status = {"required": required, "intents": 0, "completed": 0, "issues": [], "duplicate_record_numbers": [], "recovered_records": [], "bound_intents": {}, "closed": False, "unproven_intents": 0}
    issues = status["issues"]
    if not root.is_dir() and not required:
        return status
    closed = directory / "meter-closed.json"
    if not closed.is_file() or not ledger_json(closed, issues).get("incarnation"):
        issues.append("durable meter accounting is not closed")
    else:
        status["closed"] = True
    if not root.is_dir():
        issues.append("missing durable meter intents")
        return status
    by_id = {}
    for number, record in enumerate(records, 1):
        if not isinstance(record, dict) or not str(record.get("path", "")).split("?")[0].endswith(MODEL_ENDPOINTS):
            continue
        key = record.get("request_id")
        if not isinstance(key, str) or not key:
            issues.append("model request %d has no durable request identifier" % number)
        elif key in by_id:
            issues.append("duplicate durable request identifier: " + key)
            status["duplicate_record_numbers"].append(number)
        else:
            by_id[key] = checkpoint_digest(record)
    intent_ids, done_ids = set(), set()
    for path in sorted(root.iterdir()):
        if path.name.endswith(".done.json"):
            done_ids.add(path.name.removesuffix(".done.json"))
            continue
        if not path.name.endswith(".intent.json"):
            issues.append("unknown durable meter entry: " + path.name)
            continue
        key = path.name.removesuffix(".intent.json")
        intent_ids.add(key)
        intent = ledger_json(path, issues)
        done_path = root / (key + ".done.json")
        if not done_path.is_file():
            issues.append("unresolved durable meter intent: " + key)
            continue
        done = ledger_json(done_path, issues)
        if intent.get("request_id") != key or done.get("request_id") != key:
            issues.append("durable meter intent/completion identifier mismatch: " + key)
        elif not isinstance(done.get("record"), dict):
            issues.append("missing durable meter completion record: " + key)
        elif not intent.get("incarnation") or done["record"].get("meter_incarnation") != intent["incarnation"]:
            issues.append("durable meter incarnation mismatch: " + key)
        elif done["record"].get("request_id") != key or checkpoint_digest(done["record"]) != done.get("record_sha256") or (key in by_id and by_id[key] != done["record_sha256"]):
            issues.append("durable meter completion/request digest mismatch: " + key)
        elif not checkpoint_meter.bound_record(intent, done["record"], directory):
            issues.append("durable meter terminal binding mismatch: " + key)
        else:
            status["completed"] += 1
            if not proven_terminal(intent, done["record"], directory):
                status["unproven_intents"] += 1
            status["bound_intents"][key] = intent
            if key not in by_id:
                status["recovered_records"].append(done["record"])
    status["intents"] = len(intent_ids)
    for key in sorted(done_ids - intent_ids):
        issues.append("orphan durable meter completion: " + key)
    for key in sorted(set(by_id) - intent_ids):
        issues.append("orphan durable model request: " + key)
    return status


def timeout_accounting_closed(meter, ledger):
    proof = meter.get("durable_ledger") or {}
    return (proof.get("closed") is True and type(proof.get("intents")) is int
        and proof["intents"] >= 0 and type(proof.get("completed")) is int
        and proof["completed"] == proof["intents"]
        and all(type(proof.get(key)) is int and proof[key] == 0 for key in ("unknown", "unmatched_records"))
        and type(proof.get("active", 0)) is int and proof.get("active", 0) == 0 and ledger["closed"] and not ledger["issues"]
        and proof["intents"] == ledger["intents"] and proof["completed"] == ledger["completed"]
        and type(meter.get("unproven_intents")) is int and meter["unproven_intents"] == 0
        and ledger["unproven_intents"] == 0
        and type(meter.get("unknown_requests")) is int and meter["unknown_requests"] == 0
        and meter.get("accounting_status") == "observed_after_timeout_reconciliation")


def price_row(directory, row, require_ledger=False, campaign_rules=None):
    path = directory / "requests.jsonl"
    result = {"total_usd": None, "lower_bound_usd": 0.0, "unknown": [], "requests": 0, "non_model_records": 0, "not_dispatched_requests": 0, "by_model_usd": {}}
    records = read_lines(path) if path.is_file() else []
    meter = row.get("meter") or {}
    accounting = row.get("accounting_status", meter.get("accounting_status", "observed"))
    reconciled = accounting == "observed_after_timeout_reconciliation"
    result["ledger"] = meter_ledger(directory, records, require_ledger or reconciled)
    bound_intents = result["ledger"].pop("bound_intents")
    recovered = result["ledger"].pop("recovered_records")
    result["unknown"].extend(result["ledger"]["issues"])
    if not path.is_file() and not recovered:
        result["unknown"].append("missing request log")
    duplicate_records = set(result["ledger"]["duplicate_record_numbers"])
    responses_path = directory / "responses.jsonl"
    responses = read_lines(responses_path) if responses_path.is_file() else None
    if responses is not None and len(responses) != len(records):
        result["unknown"].append("request/response log lengths differ")
        responses = None
    records.extend(recovered)
    if responses is not None:
        responses.extend([None] * len(recovered))
    for index, record in enumerate(records):
        if not isinstance(record, dict) or not isinstance(record.get("path"), str):
            result["unknown"].append("record %d: missing request path" % (index + 1))
            continue
        endpoint = record["path"].split("?")[0]
        if not endpoint.endswith(MODEL_ENDPOINTS):
            result["non_model_records"] += 1
            if not endpoint.endswith("/models"):
                result["unknown"].append("record %d: unrecognized endpoint %s" % (index + 1, endpoint))
            continue
        result["requests"] += 1
        if index + 1 in duplicate_records:
            continue
        try:
            response = responses[index] if responses is not None else None
            amount, model = request_cost(record, response, row.get("id"), bound_intents.get(record.get("request_id")))
        except (ValueError, TypeError, AttributeError) as error:
            result["unknown"].append("request %d: %s" % (index + 1, error))
            continue
        if model is None:
            result["not_dispatched_requests"] += 1
            continue
        result["lower_bound_usd"] += amount
        result["by_model_usd"][model] = result["by_model_usd"].get(model, 0.0) + amount
    if reconciled and (row.get("reporting_rules") != reporting_rules.pin()
            or (campaign_rules is not None and campaign_rules != reporting_rules.pin())):
        result["unknown"].append("reconciled timeout requires matching explicit row and campaign reporting rules pins")
    if reconciled and not timeout_accounting_closed(meter, result["ledger"]):
        result["unknown"].append("timeout accounting reconciliation proof is incomplete or differs from durable evidence")
    if accounting not in ("observed", "observed_after_timeout_reconciliation") or meter.get("unknown_requests", 0) != 0:
        result["unknown"].append("incomplete request accounting: " + str(accounting))
    if meter.get("requests") != result["requests"]:
        result["unknown"].append("meter/request count mismatch or absent meter")
    if result["requests"] == result["not_dispatched_requests"]:
        result["unknown"].append("no model requests; zero cost not established")
    if not result["unknown"]:
        result["total_usd"] = result["lower_bound_usd"]
    return result


def manifest_identity(manifest):
    encoded = json.dumps(manifest, sort_keys=True, indent=1) + "\n"
    return hashlib.sha256(encoded.encode()).hexdigest()


def valid_digest(value):
    return isinstance(value, str) and len(value) == 64 and all(char in "0123456789abcdef" for char in value)


def validate_manifest(manifest, tasks, attempts, agent):
    issues = []
    if manifest.get("version") not in (1, "checkpoint-1"):
        issues.append("unsupported or missing manifest version")
    source = manifest.get("source") or {}
    commit = source.get("commit")
    if not isinstance(commit, str) or len(commit) != 40 or any(char not in "0123456789abcdef" for char in commit) or not valid_digest(source.get("tracked_sha256")):
        issues.append("missing source pins; cannot establish match")
    for field in ("agent_root_sha256", "campaign_suite_sha256", "transport_sha256"):
        if not valid_digest(manifest.get(field)):
            issues.append("missing provenance pin: " + field)
    pinned_rules = manifest.get("reporting_rules")
    if "reporting_rules" in manifest and pinned_rules != reporting_rules.pin():
        issues.append("reporting rules pin differs from current version/hash; use the pinned reporter")
    config = manifest.get("config") or {}
    for field in REQUIRED_CONFIG:
        if field not in config:
            issues.append("missing config pin: " + field)
    if config.get("agents") != [agent] or config.get("attempts") != attempts:
        issues.append("manifest agents/attempts differ from requested matrix")
    pinned = {}
    for task in manifest.get("tasks") or []:
        key = (task.get("source"), task.get("task"))
        if key in pinned:
            issues.append("duplicate manifest task: " + str(key))
        pinned[key] = task
        for field in ("package_sha256", "tree_sha256"):
            if not valid_digest(task.get(field)):
                issues.append("missing task pin: %s %s" % (key, field))
    if set(pinned) != set(tasks):
        issues.append("manifest task matrix differs from requested suite; no subsetting")
    if not manifest.get("runtime"):
        issues.append("missing runtime provenance")
    expected_ids = {"%s.%s.%s.%d" % (source, task, agent, attempt)
                    for source, task in tasks for attempt in range(1, attempts + 1)}
    if "matrix" in manifest:
        matrix = manifest["matrix"]
        if len(matrix) != len(expected_ids) or set(matrix) != expected_ids:
            issues.append("manifest row matrix differs from requested matrix")
    if "images" in manifest and set(manifest["images"]) != expected_ids:
        issues.append("manifest image matrix differs from requested matrix")
    return issues


def checkpoint_digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()).hexdigest()


def checkpoint_manifest(wrapper):
    if wrapper.get("schema") != 1:
        raise ValueError("unsupported checkpoint campaign schema")
    identity = wrapper["identity"]
    parameters = dict(identity.get("parameters") or {})
    parameters["agents"] = parameters.get("agents", "").split(",")
    parameters["ca_sha256"] = identity.get("ca_sha256")
    parameters.pop("only", None)
    tasks = []
    for key, package in sorted((identity.get("tasks") or {}).items()):
        source, separator, task = key.partition("/")
        if not separator:
            raise ValueError("checkpoint task key has no source: " + key)
        tree = (identity.get("task_trees") or {}).get(key)
        tasks.append(dict(source=source, task=task, package_sha256=package, tree_sha256=tree))
    manifest = {"version": "checkpoint-1", "source": {"commit": identity.get("source_commit"),
                "tracked_sha256": checkpoint_digest(identity["source_files"]) if identity.get("source_files") else None},
                "agent_root_sha256": checkpoint_digest(identity["agent_tree"]) if identity.get("agent_tree") else None,
                "campaign_suite_sha256": identity.get("suite_sha256"), "config": parameters,
                "tasks": tasks, "runtime": identity.get("runtime"), "transport_sha256": identity.get("transport_sha256")}
    manifest["source"]["files"] = identity.get("source_files")
    manifest["agent_tree_files"] = identity.get("agent_tree")
    manifest["measurement_source_files"] = {key: value for key, value in identity.get("source_files", {}).items()
                                            if key.startswith("evals/") and not key.endswith("report.py")}
    for field in sorted(set(identity) - {"source_commit", "source_files", "agent_tree", "parameters", "tasks", "task_trees", "suite_sha256", "ca_sha256", "runtime", "transport_sha256"}):
        manifest[field] = identity[field]
    return manifest, checkpoint_digest(identity)


def validate_checkpoint(directory, row, identity):
    path = directory / "checkpoint.json"
    if not path.is_file():
        return ["missing attempt checkpoint: " + directory.name]
    state = read_json(path)
    issues = []
    if state.get("identity") != {"campaign": identity, "row": row.get("id")}:
        issues.append("attempt checkpoint identity mismatch: " + directory.name)
    if state.get("stage") != "completed" or state.get("result_sha256") != checkpoint_digest(row):
        issues.append("attempt checkpoint result mismatch or incomplete: " + directory.name)
    return issues


def row_key(row):
    return (row.get("source"), row.get("task"), row.get("attempt"))


def validate_row(row, directory, manifest, identity, agent):
    issues = []
    if "reporting_rules" in manifest and row.get("reporting_rules") != manifest["reporting_rules"]:
        issues.append("row reporting rules pin missing or differs from campaign: " + directory.name)
    if row.get("agent") != agent:
        issues.append("unknown agent: " + str(row.get("agent")))
    if row.get("outcome") not in OUTCOMES:
        issues.append("unknown outcome: " + str(row.get("outcome")))
    if row.get("outcome") == "passed" and any((row.get(phase) or {}).get("timed_out") for phase in ("agent_result", "verifier")):
        issues.append("timed-out attempt cannot pass: " + directory.name)
    if type(row.get("attempt")) is not int:
        issues.append("attempt must be an integer")
    identifier = "%s.%s.%s.%s" % (row.get("source"), row.get("task"), agent, row.get("attempt"))
    if row.get("id") != identifier or directory.name != identifier:
        issues.append("row id/directory mismatch: " + directory.name)
    if manifest.get("version") == "checkpoint-1":
        issues.extend(validate_checkpoint(directory, row, identity))
    elif row.get("campaign_sha256") != identity:
        issues.append("row campaign provenance mismatch: " + directory.name)
    if row.get("model") != (manifest.get("config") or {}).get("model"):
        issues.append("row starting model/config mismatch: " + directory.name)
    if "images" in manifest and row.get("image") != manifest["images"].get(row.get("id")):
        issues.append("row image/manifest mismatch: " + directory.name)
    return issues


def load_arm(path, tasks, attempts, agent):
    manifest_path = path / "campaign.json"
    manifest = read_json(manifest_path) if manifest_path.is_file() else {}
    checkpoint_schema = "identity" in manifest
    if checkpoint_schema:
        manifest, identity = checkpoint_manifest(manifest)
    else:
        identity = manifest_identity(manifest) if manifest else None
    issues = validate_manifest(manifest, tasks, attempts, agent)
    rows, by_key = [], {}
    nonfinal = 0
    expected = {(source, task, attempt) for source, task in tasks for attempt in range(1, attempts + 1)}
    directories = sorted((path / "rows").iterdir()) if (path / "rows").is_dir() else []
    for directory in directories:
        result_path = directory / "result.json"
        if not result_path.is_file():
            issues.append("row entry without final result: " + directory.name)
            nonfinal += 1
            infrastructure = directory / "infrastructure.json"
            if not infrastructure.is_file():
                continue
            evidence = read_json(infrastructure)
            known = {(source + "." + task + "." + agent + "." + str(attempt)): (source, task, attempt)
                     for source, task, attempt in expected}
            if directory.name not in known:
                issues.append("unknown infrastructure row: " + directory.name)
                continue
            source, task, attempt = known[directory.name]
            row = dict(evidence, id=directory.name, source=source, task=task, agent=agent,
                       attempt=attempt, outcome="errored")
        else:
            row = read_json(result_path)
            issues.extend(validate_row(row, directory, manifest, identity, agent))
        row["pricing"] = price_row(directory, row, checkpoint_schema, manifest.get("reporting_rules", {}))
        key = row_key(row)
        if key in by_key:
            issues.append("duplicate row: " + str(key))
        by_key[key] = row
        rows.append(row)
        if key not in expected:
            issues.append("unknown row outside matrix: " + str(key))
    missing = sorted(expected - set(by_key))
    if missing:
        issues.append("%d missing rows" % len(missing))
    complete = not nonfinal and not missing and len(rows) == len(expected) and set(by_key) == expected
    return {"path": str(path), "manifest": manifest, "identity": identity, "issues": issues, "rows": rows,
            "by_key": by_key, "complete": complete, "missing": [list(key) for key in missing]}


def compare_optional(base, candidate, label, issues):
    if base != candidate:
        issues.append("mismatched " + label)


def measurement_path(path):
    if path in POLICY_FIXTURE_PATHS:
        return False
    return path.startswith(("evals/", "scripts/", "rust-toolchain")) or path in ("Cargo.toml", "Cargo.lock")


def compare_treatment(first, second, allowed_source, allowed_agent, issues):
    observed = {}
    for label, trees, digests, allowed in (
            ("source", [arm.get("source", {}).get("files") for arm in (first, second)],
             [arm.get("source", {}).get("tracked_sha256") for arm in (first, second)], allowed_source),
            ("agent", [arm.get("agent_tree_files") for arm in (first, second)],
             [arm.get("agent_root_sha256") for arm in (first, second)], allowed_agent)):
        if all(isinstance(tree, dict) for tree in trees):
            changes = sorted(key for key in set(trees[0]) | set(trees[1]) if trees[0].get(key) != trees[1].get(key))
            observed[label] = changes
            if any(not valid_digest(value) for tree in trees for value in tree.values()):
                issues.append("invalid granular " + label + " file digest")
            if label == "source" and any(measurement_path(path) for path in changes):
                issues.append("measurement harness/toolchain source changed; not a quality treatment")
            undeclared = sorted(set(changes) - set(allowed))
            if undeclared:
                issues.append("undeclared %s treatment changes: %s" % (label, ", ".join(undeclared)))
        elif digests[0] != digests[1]:
            issues.append("missing granular %s pins; cannot establish intended-only treatment delta" % label)
            observed[label] = None
        else:
            observed[label] = []
    return {"declared_source_changes": allowed_source, "declared_agent_changes": allowed_agent,
            "observed_changes": observed}


def compare_arms(base, candidate, tasks, attempts, allowed_source, allowed_agent):
    issues = []
    first, second = base["manifest"], candidate["manifest"]
    treatment = compare_treatment(first, second, allowed_source, allowed_agent, issues)
    for field in sorted((set(first) | set(second)) - {"source", "agent_root_sha256", "agent_tree_files", "config", "tasks"}):
        compare_optional(first.get(field), second.get(field), "manifest " + field, issues)
    configs = [{key: value for key, value in arm.get("config", {}).items() if key not in BINDING_FIELDS} for arm in (first, second)]
    compare_optional(*configs, "config", issues)
    compare_optional(first.get("tasks"), second.get("tasks"), "task pins/config source", issues)
    for task in tasks:
        for attempt in range(1, attempts + 1):
            key = task + (attempt,)
            rows = [arm["by_key"].get(key) for arm in (base, candidate)]
            if any(row is None for row in rows):
                continue
            fields = {field for row in rows for field in row if any(word in field for word in ("image", "cache", "provenance", "task_sha", "package_sha", "tree_sha"))}
            for field in sorted(fields):
                compare_optional(rows[0].get(field), rows[1].get(field), "%s %s" % (key, field), issues)
            if any(not row.get("image") for row in rows):
                issues.append("missing image pin: " + str(key))
    return issues, treatment


def accounting_evidence(arm, path):
    arm["accounting_evidence"] = None
    if path is None:
        automatic = Path(arm["path"]) / "accounting.json"
        path = automatic if automatic.is_file() else None
    if path is None:
        return
    value = json.loads(path.read_text())
    records = value if isinstance(value, list) else value.get("streams", [])
    unknown = [record for record in records if record.get("aborted_cost_unknown") or record.get("unknown_requests")]
    arm["accounting_evidence"] = {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                                  "unknown_streams": unknown}


def arm_summary(arm, expected):
    rows = arm["rows"]
    counts = {outcome: sum(row.get("outcome") == outcome for row in rows) for outcome in OUTCOMES}
    counts["invalid"] = len(rows) - sum(counts.values())
    passed = counts["passed"]
    unknown = [row.get("id") for row in rows if row["pricing"]["total_usd"] is None]
    evidence = arm.get("accounting_evidence")
    external_unknown = evidence["unknown_streams"] if evidence else []
    full_cost = arm["complete"] and not unknown and not external_unknown
    lower_bound = sum(row["pricing"]["lower_bound_usd"] for row in rows)
    provenance = {field: arm["manifest"].get(field) for field in
                  ("source", "agent_root_sha256", "config", "tasks", "runtime", "transport_sha256", "cache", "reporting_rules")}
    provenance["campaign_sha256"] = arm["identity"]
    return {"path": arm["path"], "provenance": provenance, "complete": arm["complete"], "expected_attempts": expected,
            "observed_rows": len(rows), "counts": counts, "missing": arm["missing"],
            "pass_rate": passed / expected, "pass_wilson_95": wilson(passed, expected) if arm["complete"] else None,
            "cost_total_usd": lower_bound if full_cost else None, "cost_lower_bound_usd": lower_bound,
            "cost_mean_usd": lower_bound / expected if full_cost else None,
            "cost_mean_bootstrap_95": None,
            "cost_per_pass": lower_bound / passed if full_cost and passed else None,
            "cost_per_pass_bootstrap_95": None,
            "campaign_rules_status": "pinned" if arm["manifest"].get("reporting_rules") == reporting_rules.pin() else "legacy_unpinned" if "reporting_rules" not in arm["manifest"] else "pin_mismatch",
            "cost_mean_lower_bound_usd": lower_bound / expected if arm["complete"] else None,
            "unknown_cost_rows": unknown, "unknown_external_streams": external_unknown,
            "accounting_evidence": evidence, "issues": arm["issues"],
            "rows": [{"id": row.get("id"), "outcome": row.get("outcome"), "reporting_rules": row.get("reporting_rules"), "pricing": row["pricing"]} for row in rows]}


def percentile(values, probability):
    position = (len(values) - 1) * probability
    lower = math.floor(position)
    fraction = position - lower
    return values[lower] * (1 - fraction) + values[math.ceil(position)] * fraction


def arm_cost_bootstrap(arm, summary, tasks, attempts, repetitions, seed):
    if summary["cost_mean_usd"] is None:
        return None
    clusters = [sum(arm["by_key"][task + (attempt,)]["pricing"]["total_usd"]
                    for attempt in range(1, attempts + 1)) / attempts for task in tasks]
    count = len(clusters)
    randomizer = random.Random(seed)
    samples = sorted(sum(clusters[randomizer.randrange(count)] for _ in range(count)) / count
                     for _ in range(repetitions))
    return {"method": "task-cluster percentile bootstrap; all attempts retained per sampled task",
            "clusters": count, "attempts_per_cluster": attempts, "repetitions": repetitions, "seed": seed,
            "mean": summary["cost_mean_usd"], "ci_95": [percentile(samples, 0.025), percentile(samples, 0.975)]}


def cost_per_pass_bootstrap(arm, summary, tasks, attempts):
    if summary["cost_per_pass"] is None:
        return None
    clusters = []
    for task in tasks:
        rows = [arm["by_key"][task + (attempt,)] for attempt in range(1, attempts + 1)]
        clusters.append((sum(row["pricing"]["total_usd"] for row in rows),
                         sum(row["outcome"] == "passed" for row in rows)))
    count = len(clusters)
    randomizer = random.Random(reporting_rules.COST_PER_PASS_SEED)
    samples, zero_pass = [], 0
    for _ in range(reporting_rules.COST_PER_PASS_REPETITIONS):
        selected = [clusters[randomizer.randrange(count)] for _ in range(count)]
        passed = sum(cluster[1] for cluster in selected)
        zero_pass += int(passed == 0)
        samples.append(sum(cluster[0] for cluster in selected) / passed if passed else math.inf)
    samples.sort()
    endpoints = []
    for probability in (0.025, 0.975):
        position = (len(samples) - 1) * probability
        lower, upper = samples[math.floor(position)], samples[math.ceil(position)]
        fraction = position - math.floor(position)
        value = lower if fraction == 0 else lower * (1 - fraction) + upper * fraction
        endpoints.append(value if math.isfinite(value) else None)
    return {"method": "task-cluster percentile bootstrap of total all-attempt cost / passes",
            "clusters": count, "attempts_per_cluster": attempts,
            "repetitions": reporting_rules.COST_PER_PASS_REPETITIONS, "seed": reporting_rules.COST_PER_PASS_SEED,
            "mean": summary["cost_per_pass"], "ci_95": endpoints,
            "zero_pass_replicates": zero_pass,
            "zero_pass_policy": "retained as positive infinity; null CI endpoint means unbounded"}


def report_rules(args):
    return {"pin": reporting_rules.pin(), "manifest": reporting_rules.manifest(),
            "per_attempt_bootstrap": {"repetitions": args.bootstrap_reps, "seed": args.seed},
            "historical_policy": "current rules applied explicitly; legacy unpinned campaigns are not relabelled or modified"}


def clustered_bootstrap(base, candidate, tasks, attempts, repetitions, seed):
    pass_clusters, cost_clusters = [], []
    cost_known = True
    for task in tasks:
        pass_delta, cost_delta = 0.0, 0.0
        for attempt in range(1, attempts + 1):
            key = task + (attempt,)
            first, second = base["by_key"][key], candidate["by_key"][key]
            pass_delta += int(second["outcome"] == "passed") - int(first["outcome"] == "passed")
            costs = [row["pricing"]["total_usd"] for row in (first, second)]
            if None in costs:
                cost_known = False
            else:
                cost_delta += costs[1] - costs[0]
        pass_clusters.append(pass_delta / attempts)
        cost_clusters.append(cost_delta / attempts)
    randomizer = random.Random(seed)
    pass_samples, cost_samples = [], []
    count = len(tasks)
    for _ in range(repetitions):
        selected = [randomizer.randrange(count) for _ in range(count)]
        pass_samples.append(sum(pass_clusters[index] for index in selected) / count)
        if cost_known:
            cost_samples.append(sum(cost_clusters[index] for index in selected) / count)
    def estimate(values, samples):
        return {"mean": sum(values) / count, "ci_95": [percentile(sorted(samples), 0.025), percentile(sorted(samples), 0.975)]}
    return {"method": "paired task-cluster percentile bootstrap; candidate minus baseline",
            "clusters": count, "attempts_per_cluster": attempts, "repetitions": repetitions, "seed": seed,
            "pass_difference": estimate(pass_clusters, pass_samples),
            "cost_difference_usd": estimate(cost_clusters, cost_samples) if cost_known else None}


def build_report(args):
    tasks = read_suite(args.suite)
    if len(tasks) != args.expected_tasks:
        raise ValueError("suite has %d tasks; expected %d" % (len(tasks), args.expected_tasks))
    base = load_arm(args.baseline, tasks, args.attempts, args.agent)
    candidate = load_arm(args.candidate, tasks, args.attempts, args.agent)
    accounting_evidence(base, getattr(args, "baseline_accounting", None))
    accounting_evidence(candidate, getattr(args, "candidate_accounting", None))
    issues, treatment = compare_arms(base, candidate, tasks, args.attempts,
                                    getattr(args, "allow_source_change", []), getattr(args, "allow_agent_change", []))
    if args.baseline.resolve() == args.candidate.resolve():
        issues.append("baseline and candidate must be distinct arms")
    matched = not (issues or base["issues"] or candidate["issues"])
    expected = len(tasks) * args.attempts
    baseline_summary, candidate_summary = [arm_summary(arm, expected) for arm in (base, candidate)]
    for arm, summary in ((base, baseline_summary), (candidate, candidate_summary)):
        summary["cost_mean_bootstrap_95"] = arm_cost_bootstrap(arm, summary, tasks, args.attempts, args.bootstrap_reps, args.seed)
        summary["cost_per_pass_bootstrap_95"] = cost_per_pass_bootstrap(arm, summary, tasks, args.attempts)
    paired = clustered_bootstrap(base, candidate, tasks, args.attempts, args.bootstrap_reps, args.seed) if matched else None
    if paired and (baseline_summary["cost_total_usd"] is None or candidate_summary["cost_total_usd"] is None):
        paired["cost_difference_usd"] = None
    gates = {"complete_matched": matched and base["complete"] and candidate["complete"],
             "both_costs_known": baseline_summary["cost_total_usd"] is not None and candidate_summary["cost_total_usd"] is not None,
             "candidate_success": candidate_summary["pass_rate"] >= args.min_success,
             "candidate_cost": candidate_summary["cost_mean_usd"] is not None and candidate_summary["cost_mean_usd"] <= args.max_cost,
             "paired_improvement_ci_positive": paired is not None and paired["pass_difference"]["ci_95"][0] > 0,
             "quality_improvement": paired is not None and paired["pass_difference"]["mean"] + 0.000000000001 >= args.min_improvement_pp / 100}
    canonical = read_suite(Path(__file__).with_name("suite-60.txt"))
    profile = "MATCHED180" if tasks == canonical and args.attempts == 3 else "CUSTOM_%d_TASKS_%d_ATTEMPTS_NOT_MATCHED180" % (len(tasks), expected)
    return {"schema_version": 2, "reporting_rules": report_rules(args), "profile": profile, "matched": matched, "status": "PASS" if all(gates.values()) else "FAIL",
            "experiment": "quality-first: model/effort/finish-check/budget configuration stays matched. Intentional model/cost-cascade/finish-check config changes require a separate cost experiment; this report does not exempt them.",
            "policy": {"gates_apply_to": "candidate; both arms require complete matched matrices", "min_success": args.min_success, "max_cost_usd_per_attempt": args.max_cost, "min_improvement_pp": args.min_improvement_pp, "paired_improvement_ci_lower_strictly_above": 0.0},
            "gates": gates, "issues": issues, "treatment": treatment, "baseline": baseline_summary, "candidate": candidate_summary,
            "paired": paired, "prices_per_million_tokens": PRICES_PER_MTOK, "prices_source": PRICES_SOURCE,
            "interpretation": "Errors count as nonpasses in the full planned denominator. Cost is list-price equivalent, not invoiced spend. Unknown costs are not zero. Candidate source and agent-root pins may differ only at declared treatment paths with granular pins; measurement config, task, image and cache variants must match."}


def build_single_arm_report(args):
    tasks = read_suite(args.suite)
    if len(tasks) != args.expected_tasks:
        raise ValueError("suite has %d tasks; expected %d" % (len(tasks), args.expected_tasks))
    arm = load_arm(args.single_arm, tasks, args.attempts, args.agent)
    accounting_evidence(arm, getattr(args, "accounting", None))
    summary = arm_summary(arm, len(tasks) * args.attempts)
    summary["cost_mean_bootstrap_95"] = arm_cost_bootstrap(arm, summary, tasks, args.attempts, args.bootstrap_reps, args.seed)
    summary["cost_per_pass_bootstrap_95"] = cost_per_pass_bootstrap(arm, summary, tasks, args.attempts)
    complete = arm["complete"] and not arm["issues"] and summary["cost_total_usd"] is not None
    return {"schema_version": 2, "mode": "single_arm", "profile": "SINGLE_ARM_%d_TASKS_%d_ATTEMPTS_NOT_MATCHED" % (len(tasks), len(tasks) * args.attempts),
            "status": "COMPLETE" if complete else "INCOMPLETE", "reporting_rules": report_rules(args),
            "arm": summary, "matched": False, "paired": None,
            "interpretation": "Descriptive single-arm result only; no matched comparison or adoption claim. Missing or unknown evidence blocks a complete result. Legacy unpinned campaigns are not relabelled."}


def readable(report):
    lines = [report["profile"] + ": " + report["status"],
             "Reporting rules: " + json.dumps(report["reporting_rules"]["pin"], sort_keys=True)]
    scopes = ("arm",) if report.get("mode") == "single_arm" else ("baseline", "candidate")
    for name in scopes:
        arm = report[name]
        cost_label = "$%.6f/attempt" % arm["cost_mean_usd"] if arm["cost_mean_usd"] is not None else "unknown cost (known lower bound $%.6f total)" % arm["cost_lower_bound_usd"]
        cost_interval = arm["cost_mean_bootstrap_95"]
        if cost_interval:
            cost_label += " [95%% task-cluster $%.6f, $%.6f; %d clusters x %d attempts]" % (*cost_interval["ci_95"], cost_interval["clusters"], cost_interval["attempts_per_cluster"])
        ratio = arm["cost_per_pass"]
        cost_label += "; " + ("$%.6f/pass (all attempts)" % ratio if ratio is not None else "unknown cost/pass")
        ratio_interval = arm["cost_per_pass_bootstrap_95"]
        if ratio_interval:
            endpoints = ["unbounded" if value is None else "$%.6f" % value for value in ratio_interval["ci_95"]]
            cost_label += " [95%% %s, %s; %d zero-pass replicates]" % (*endpoints, ratio_interval["zero_pass_replicates"])
        cost_label += "; campaign rules: " + arm["campaign_rules_status"]
        interval = arm["pass_wilson_95"]
        confidence = " [95%% Wilson %.2f%%, %.2f%%]" % (100 * interval[0], 100 * interval[1]) if interval else ""
        lines.append("%s: %d/%d passed (%.2f%%)%s; %d failed, %d errors, %d missing; %s; %d unknown-cost rows" % (name, arm["counts"]["passed"], arm["expected_attempts"], 100 * arm["pass_rate"], confidence, arm["counts"]["failed"], arm["counts"]["errored"], len(arm["missing"]), cost_label, len(arm["unknown_cost_rows"])))
    paired = report["paired"]
    if paired:
        difference = paired["pass_difference"]
        lines.append("Paired delta: %.2f pp [95%% %.2f, %.2f]; %d task clusters, %d repetitions, seed %d" % (100 * difference["mean"], 100 * difference["ci_95"][0], 100 * difference["ci_95"][1], paired["clusters"], paired["repetitions"], paired["seed"]))
        lines.append("Paired cost delta: " + (json.dumps(paired["cost_difference_usd"]) if paired["cost_difference_usd"] is not None else "unknown"))
    for name in scopes:
        if report[name]["unknown_external_streams"]:
            lines.append("%s: %d preserved streams with unknown cost" % (name, len(report[name]["unknown_external_streams"])))
    if report.get("mode") == "single_arm":
        lines.append(report["interpretation"])
        lines.extend(report["arm"]["issues"])
        return "\n".join(lines)
    lines.append("Adoption requires paired mean improvement >= %.2f pp and paired 95%% CI lower bound > 0 pp." % report["policy"]["min_improvement_pp"])
    for name, passed in report["gates"].items():
        lines.append(name + ": " + ("PASS" if passed else "FAIL"))
    for scope in ("baseline", "candidate"):
        lines.extend(scope + ": " + issue for issue in report[scope]["issues"])
    lines.extend(report["issues"])
    return "\n".join(lines)


def arguments():
    parser = argparse.ArgumentParser(description="Strict matched baseline/candidate arm report; no row filtering or inferred provenance.")
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--candidate", type=Path)
    parser.add_argument("--single-arm", type=Path, help="descriptive report for one campaign, never a matched adoption result")
    parser.add_argument("--accounting", type=Path, help="single-arm preserved stream accounting JSON")
    parser.add_argument("--baseline-accounting", type=Path, help="preserved stream accounting JSON (including aborted streams)")
    parser.add_argument("--candidate-accounting", type=Path, help="preserved stream accounting JSON (including aborted streams)")
    parser.add_argument("--allow-source-change", action="append", default=[], help="declare one exact intended treatment source path; repeat per changed file")
    parser.add_argument("--allow-agent-change", action="append", default=[], help="declare one exact intended treatment agent-tree path, such as bin/hl-agent")
    parser.add_argument("--suite", type=Path, default=Path(__file__).with_name("suite-60.txt"))
    parser.add_argument("--attempts", type=int, default=3)
    parser.add_argument("--expected-tasks", type=int, default=60)
    parser.add_argument("--agent", choices=("hl-agent",), default="hl-agent")
    parser.add_argument("--min-success", type=float, default=0.92)
    parser.add_argument("--max-cost", type=float, default=0.010)
    parser.add_argument("--min-improvement-pp", type=float, default=5.0, help="candidate minus baseline point-estimate percentage points; not relative percent")
    parser.add_argument("--bootstrap-reps", type=int, default=10000)
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--json-out", type=Path)
    parser.add_argument("--summary", action="store_true", help="print readable summary instead of JSON")
    args = parser.parse_args()
    if args.single_arm is not None:
        if args.baseline is not None or args.candidate is not None or args.baseline_accounting or args.candidate_accounting or args.allow_source_change or args.allow_agent_change:
            parser.error("--single-arm cannot be combined with matched-arm options")
    elif args.baseline is None or args.candidate is None or args.accounting:
        parser.error("supply --baseline and --candidate, or --single-arm [--accounting]")
    for path in args.allow_source_change + args.allow_agent_change:
        if not path or Path(path).is_absolute() or ".." in Path(path).parts or any(char in path for char in "*?["):
            parser.error("treatment declarations must be exact safe relative paths")
    if any(measurement_path(path) for path in args.allow_source_change):
        parser.error("measurement harness and toolchain changes cannot be treatment exceptions")
    if min(args.attempts, args.expected_tasks, args.bootstrap_reps) < 1:
        parser.error("attempts, expected-tasks and bootstrap-reps must be positive")
    if not 0 <= args.min_success <= 1 or not math.isfinite(args.max_cost) or args.max_cost < 0 or not 0 <= args.min_improvement_pp <= 100:
        parser.error("invalid quality/cost policy")
    return args


def main():
    args = arguments()
    try:
        report = build_single_arm_report(args) if args.single_arm is not None else build_report(args)
    except (OSError, ValueError, TypeError, KeyError) as error:
        print("report input error: " + str(error), file=sys.stderr)
        return 2
    encoded = json.dumps(report, sort_keys=True, indent=2, allow_nan=False) + "\n"
    if args.json_out:
        args.json_out.write_text(encoded)
    print(readable(report) if args.summary else encoded, end="\n" if args.summary else "")
    return 0 if report["status"] in ("PASS", "COMPLETE") else 1


if __name__ == "__main__":
    raise SystemExit(main())
