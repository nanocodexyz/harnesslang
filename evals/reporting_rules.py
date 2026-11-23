import copy
import hashlib
import json
from pathlib import Path

VERSION = "closed-accounting-v3-budgeted"
COST_PER_PASS_REPETITIONS = 10000
COST_PER_PASS_SEED = 0
PRICES_PER_MTOK = {
    "claude-sonnet-4-6": {"input": 3.0, "output": 15.0, "cache_write_5m": 3.75, "cache_write_1h": 6.0, "cache_read": 0.30},
    "claude-haiku-4-5": {"input": 1.0, "output": 5.0, "cache_write_5m": 1.25, "cache_write_1h": 2.0, "cache_read": 0.10},
    "gpt-5.5": {"input": 5.0, "output": 30.0, "cache_write_5m": 0.0, "cache_write_1h": 0.0, "cache_read": 0.50},
    "gpt-5.6-sol": {"input": 4.0, "output": 20.0, "cache_write_5m": 0.0, "cache_write_1h": 0.0, "cache_read": 0.40},
    "gpt-6-luna": {"input": 0.10, "output": 0.50, "cache_write_5m": 0.0, "cache_write_1h": 0.0, "cache_read": 0.01},
    "gpt-6.1-terra": {"input": 1.0, "output": 5.0, "cache_write_5m": 0.0, "cache_write_1h": 0.0, "cache_read": 0.05},
}
PRICES_SOURCE = "https://docs.anthropic.com/en/docs/about-claude/pricing, read 2026-11-14; https://developers.openai.com/api/docs/models/gpt-5.5 (prompts under 272K tokens), read 2026-11-15; gpt-5.6-sol at https://openrouter.ai/openai/gpt-5.6-sol, read 2026-11-16 (a notional list price: the ChatGPT plan bills none); gpt-6-luna and gpt-6.1-terra at the standard-context list rates agent/src/openai.rs carries"

RULES = {
    "version": VERSION,
    "prices_per_million_tokens": PRICES_PER_MTOK,
    "prices_source": PRICES_SOURCE,
    "cost_basis": "notional list-price equivalent USD; every attempt including failed and errored attempts",
    "cost_per_pass": {
        "point": "sum(all attempt costs) / number of passed attempts; null for unknown cost, incomplete matrix or zero passes",
        "interval": "95% task-cluster percentile bootstrap of ratio(sum(cost), sum(passes)); sample tasks with replacement, retain every attempt within task",
        "repetitions": COST_PER_PASS_REPETITIONS,
        "seed": COST_PER_PASS_SEED,
        "random": "Python random.Random(seed).randrange(task_count), task_count draws per replicate, sorted source/task clusters",
        "percentile": "linear interpolation at (repetitions - 1) * probability; probabilities 0.025 and 0.975",
        "zero_pass_replicates": "retain as positive infinity even at zero cost; report count; null CI endpoint means unbounded, never silently drop",
        "acceptance_profile": {"tasks": 60, "attempts_per_task": 3},
    },
    "pass_interval": "95% Wilson score, z=1.959964, all planned attempts; no interval for incomplete matrix",
    "per_attempt_cost": "existing total cost / planned attempts and configurable task-cluster percentile bootstrap retained",
    "timeout": "closed agent deadline is failed and verifier deadline is errored, never passed; retain verified observed usage only with observed_after_timeout_reconciliation, persisted complete closure proof and matching live durable ledger; unresolved dispatch, missing terminal evidence, incarnation change or incomplete closure remains unknown/infrastructure",
    "accounting": "closed durable ledger, bound unique request receipts, observed usage and model price required; unknown is never zero; legacy inputs remain explicitly unpinned",
    "dispatch_budget": "optional pinned campaign list-price-equivalent cap; reserve conservative platform bounds durably before dispatch, settle known terminal usage, retain unresolved reservations and halt on uncertainty; no task or verifier modification",
    "matched_gates": "complete matched arms; both costs known; candidate pass rate >= min_success; candidate per-attempt cost <= max_cost; paired pass delta >= min_improvement_pp and paired 95% lower bound > 0",
}


def manifest():
    value = copy.deepcopy(RULES)
    root = Path(__file__).resolve().parent
    value["implementation_sha256"] = {name: hashlib.sha256((root / name).read_bytes()).hexdigest()
                                      for name in ("report.py", "reporting_rules.py", "checkpoint_meter.py", "checkpoint.py", "run.py", "checkpoint_phase.py", "provenance.py", "meter.py", "budget.py")}
    return value


def pin():
    encoded = json.dumps(manifest(), sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
    return {"version": VERSION, "sha256": hashlib.sha256(encoded).hexdigest()}
