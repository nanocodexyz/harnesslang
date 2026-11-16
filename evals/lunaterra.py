import glob
import json
import os
import sys

from report import PRICES_PER_MTOK, TOKENS_PER_MTOK, sign_test, wilson

SCORED = ("passed", "failed")
USAGE_PRICES = (("input_tokens", "input"), ("cache_read_tokens", "cache_read"), ("output_tokens", "output"))


def request_cost(model, usage):
    price = PRICES_PER_MTOK[model]
    return sum(usage.get(field, 0) * price[rate] for field, rate in USAGE_PRICES) / TOKENS_PER_MTOK


def row_cost(row_dir, result):
    path = os.path.join(row_dir, "requests.jsonl")
    if not os.path.isfile(path):
        return {result["model"]: 0} if result["outcome"] not in SCORED else None, 0.0
    models = {}
    total = 0.0
    for line in open(path):
        record = json.loads(line)
        model = record["request"]["model"]
        models[model] = models.get(model, 0) + 1
        total += request_cost(model, record.get("usage") or {})
    return models, total


def load(rows_dir):
    rows = {}
    for path in sorted(glob.glob(os.path.join(rows_dir, "*", "result.json"))):
        result = json.load(open(path))
        models, cost = row_cost(os.path.dirname(path), result)
        assert models is not None, path + " was scored without requests.jsonl"
        key = (result["source"], result["task"], result["attempt"])
        rows[key] = {"outcome": result["outcome"], "cost": cost, "models": models}
    return rows


def block(name, rows):
    scored = [row for row in rows if row["outcome"] in SCORED]
    passed = sum(row["outcome"] == "passed" for row in scored)
    escalated = [row for row in scored if len(row["models"]) > 1]
    low, high = wilson(passed, len(scored))
    cost = sum(row["cost"] for row in scored) / len(scored) if scored else float("nan")
    return [
        "%s: %d rows, %d scored, %d errored" % (name, len(rows), len(scored), len(rows) - len(scored)),
        "  passed %d/%d = %.1f%% (Wilson 95%% %.1f to %.1f)" % (passed, len(scored), 100 * passed / len(scored) if scored else float("nan"), 100 * low, 100 * high),
        "  $%.4f per scored attempt, $%.4f in all" % (cost, sum(row["cost"] for row in rows)),
        "  rows on more than one model: %d, %d of them passed" % (len(escalated), sum(row["outcome"] == "passed" for row in escalated)),
    ]


def paired(run, base):
    keys = sorted(key for key in run if key in base and run[key]["outcome"] in SCORED and base[key]["outcome"] in SCORED)
    both = sum(run[key]["outcome"] == "passed" and base[key]["outcome"] == "passed" for key in keys)
    neither = sum(run[key]["outcome"] == "failed" and base[key]["outcome"] == "failed" for key in keys)
    only_run = [key for key in keys if run[key]["outcome"] == "passed" and base[key]["outcome"] == "failed"]
    only_base = [key for key in keys if run[key]["outcome"] == "failed" and base[key]["outcome"] == "passed"]
    lines = block("run on the paired rows", [run[key] for key in keys]) + block("baseline on the paired rows", [base[key] for key in keys])
    lines.append("Paired by source, task and attempt: %d pairs, both passed %d, both failed %d, only run %d, only baseline %d, sign test p = %.3f" % (len(keys), both, neither, len(only_run), len(only_base), sign_test(len(only_run), len(only_base))))
    lines.append("Only run passed: " + (", ".join("%s.%d" % (key[1], key[2]) for key in only_run) or "none"))
    lines.append("Only baseline passed: " + (", ".join("%s.%d" % (key[1], key[2]) for key in only_base) or "none"))
    return lines


def main():
    if len(sys.argv) not in (2, 3):
        sys.exit("usage: lunaterra.py RUN_ROWS [BASELINE_ROWS]")
    run = load(sys.argv[1])
    lines = block("run", list(run.values()))
    if len(sys.argv) == 3:
        base = load(sys.argv[2])
        lines += block("baseline", list(base.values())) + paired(run, base)
    print("\n".join(lines))


if __name__ == "__main__":
    main()
