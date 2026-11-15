import glob
import json
import math
import os
import sys

WILSON_Z_95 = 1.959964
PRICES_PER_MTOK = {
    "claude-sonnet-4-6": {"input": 3.0, "output": 15.0, "cache_write_5m": 3.75, "cache_write_1h": 6.0, "cache_read": 0.30},
    "claude-haiku-4-5": {"input": 1.0, "output": 5.0, "cache_write_5m": 1.25, "cache_write_1h": 2.0, "cache_read": 0.10},
    "gpt-5.5": {"input": 5.0, "output": 30.0, "cache_write_5m": 0.0, "cache_write_1h": 0.0, "cache_read": 0.50},
}
PRICES_SOURCE = "https://docs.anthropic.com/en/docs/about-claude/pricing, read 2026-11-14; https://developers.openai.com/api/docs/models/gpt-5.5 (prompts under 272K tokens), read 2026-11-15"
CODEX_TOOL_ITEMS = ("command_execution", "file_change", "mcp_tool_call", "web_search")
TOKENS_PER_MTOK = 1_000_000
INNER_TOOL_MARK = "\u00b7 "
CODE_MODE_TOOLS = ("exec", "wait")


def wilson(passed, total):
    if total == 0:
        return (float("nan"), float("nan"))
    p = passed / total
    denominator = 1 + WILSON_Z_95 ** 2 / total
    centre = (p + WILSON_Z_95 ** 2 / (2 * total)) / denominator
    half = WILSON_Z_95 * math.sqrt(p * (1 - p) / total + WILSON_Z_95 ** 2 / (4 * total * total)) / denominator
    return (centre - half, centre + half)


def cost(model, meter):
    price = PRICES_PER_MTOK[model]
    five_minute = meter["cache_write_tokens"] - meter.get("cache_write_1h_tokens", 0)
    return (
        meter["input_tokens"] * price["input"]
        + meter["output_tokens"] * price["output"]
        + five_minute * price["cache_write_5m"]
        + meter.get("cache_write_1h_tokens", 0) * price["cache_write_1h"]
        + meter["cache_read_tokens"] * price["cache_read"]
    ) / TOKENS_PER_MTOK


def codex_tool_calls(path):
    if not os.path.isfile(path):
        return None
    count = 0
    for line in open(path, errors="replace"):
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if event.get("type") == "item.completed" and (event.get("item") or {}).get("type") in CODEX_TOOL_ITEMS:
            count += 1
    return count


def inner_tool_calls(row_dir, result):
    if result["agent"].startswith("hl-agent"):
        return (result.get("self_reported_usage") or {}).get("tool_calls")
    if result["agent"] == "codex":
        return codex_tool_calls(os.path.join(row_dir, "journal", "agent.stdout"))
    path = os.path.join(row_dir, "journal", "agent.stderr")
    if not os.path.isfile(path):
        return None
    count = 0
    for line in open(path, errors="replace"):
        if line.startswith(INNER_TOOL_MARK) and line[len(INNER_TOOL_MARK):].strip() not in CODE_MODE_TOOLS:
            count += 1
    return count


def load(argument):
    out, _, label = argument.partition("=")
    rows = []
    for path in sorted(glob.glob(os.path.join(out, "rows", "*", "result.json"))):
        result = json.load(open(path))
        meter = result.get("meter") or {}
        result["cost"] = cost(result["model"], meter) if meter else 0.0
        result["inner_tools"] = inner_tool_calls(os.path.dirname(path), result)
        if label and result["agent"] != "nanocode":
            result["agent"] = label
        rows.append(result)
    return rows


def mean(values):
    values = [value for value in values if value is not None]
    return sum(values) / len(values) if values else float("nan")


def summary(rows, agents):
    lines = ["| Agent | Attempts | Passed | Failed | Errored | Pass rate (95% Wilson) | Cost | Cost/attempt | Wall/attempt | Requests/attempt | Model tool calls/attempt | Inner tool calls/attempt | Input+cache tokens/attempt | Output tokens/attempt |", "| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for agent in agents:
        mine = [row for row in rows if row["agent"] == agent]
        passed = sum(row["outcome"] == "passed" for row in mine)
        failed = sum(row["outcome"] == "failed" for row in mine)
        errored = sum(row["outcome"] == "errored" for row in mine)
        scored = passed + failed
        low, high = wilson(passed, scored)
        rate = passed / scored if scored else float("nan")
        meters = [row.get("meter") or {} for row in mine]
        prompt = [m.get("input_tokens", 0) + m.get("cache_read_tokens", 0) + m.get("cache_write_tokens", 0) for m in meters]
        lines.append(
            "| %s | %d | %d | %d | %d | %.1f%% (%.1f-%.1f%%) | $%.2f | $%.3f | %.0f s | %.1f | %.1f | %.1f | %.0f | %.0f |"
            % (
                agent, len(mine), passed, failed, errored, 100 * rate, 100 * low, 100 * high,
                sum(row["cost"] for row in mine), mean([row["cost"] for row in mine]),
                mean([(row.get("agent_result") or {}).get("wall_ms", 0) / 1000 for row in mine]),
                mean([m.get("requests") for m in meters]), mean([m.get("tool_calls") for m in meters]),
                mean([row["inner_tools"] for row in mine]), mean(prompt), mean([m.get("output_tokens") for m in meters]),
            )
        )
    return lines


def per_task(rows, agents):
    tasks = sorted({(row["source"], row["task"]) for row in rows})
    header = "| Task | " + " | ".join(agent + " pass | " + agent + " $ | " + agent + " s | " + agent + " req | " + agent + " tools" for agent in agents) + " |"
    lines = [header, "| --- |" + " ---: |" * (5 * len(agents))]
    for source, task in tasks:
        cells = []
        for agent in agents:
            mine = [row for row in rows if row["agent"] == agent and row["task"] == task and row["source"] == source]
            passed = sum(row["outcome"] == "passed" for row in mine)
            errored = sum(row["outcome"] == "errored" for row in mine)
            mark = "%d/%d" % (passed, len(mine) - errored) + (" (%d err)" % errored if errored else "")
            cells += [
                mark,
                "%.3f" % mean([row["cost"] for row in mine]),
                "%.0f" % mean([(row.get("agent_result") or {}).get("wall_ms", 0) / 1000 for row in mine]),
                "%.1f" % mean([(row.get("meter") or {}).get("requests") for row in mine]),
                "%.1f" % mean([row["inner_tools"] for row in mine]),
            ]
        lines.append("| " + task + " | " + " | ".join(cells) + " |")
    return lines


def paired(rows, agents):
    if len(agents) != 2:
        return []
    first, second = agents
    wins = {first: [], second: []}
    for task in sorted({row["task"] for row in rows}):
        rate = {}
        for agent in agents:
            scored = [row for row in rows if row["task"] == task and row["agent"] == agent and row["outcome"] in ("passed", "failed")]
            rate[agent] = sum(row["outcome"] == "passed" for row in scored) / len(scored) if scored else None
        if rate[first] is None or rate[second] is None:
            continue
        if rate[first] > rate[second]:
            wins[first].append(task)
        elif rate[second] > rate[first]:
            wins[second].append(task)
    return [
        "Tasks where %s passes more often: %s" % (first, ", ".join(wins[first]) or "none"),
        "Tasks where %s passes more often: %s" % (second, ", ".join(wins[second]) or "none"),
    ]


def interval(values):
    count = len(values)
    centre = sum(values) / count
    if count < 2:
        return centre, float("nan"), float("nan")
    spread = math.sqrt(sum((value - centre) ** 2 for value in values) / (count - 1) / count)
    return centre, centre - WILSON_Z_95 * spread, centre + WILSON_Z_95 * spread


def differences(rows, agents):
    if len(agents) != 2:
        return []
    first, second = agents
    by_key = {(row["source"], row["task"], row["attempt"], row["agent"]): row for row in rows}
    pairs = [(by_key[key[:3] + (first,)], row) for key, row in by_key.items() if key[3] == second and key[:3] + (first,) in by_key]
    if not pairs:
        return []
    measures = (
        ("cost ($)", lambda row: row["cost"], "%.4f"),
        ("wall (s)", lambda row: (row.get("agent_result") or {}).get("wall_ms", 0) / 1000, "%.1f"),
        ("requests", lambda row: (row.get("meter") or {}).get("requests", 0), "%.2f"),
        ("prompt tokens", lambda row: sum((row.get("meter") or {}).get(key, 0) for key in ("input_tokens", "cache_read_tokens", "cache_write_tokens")), "%.0f"),
    )
    lines = ["Paired by task and attempt (%d pairs), %s minus %s, mean with its 95%% normal interval:" % (len(pairs), second, first)]
    for name, measure, form in measures:
        centre, low, high = interval([measure(other) - measure(base) for base, other in pairs])
        lines.append(("- %s: " + form + " (" + form + " to " + form + ")") % (name, centre, low, high))
    return lines


def sign_test(wins, losses):
    count = wins + losses
    if count == 0:
        return 1.0
    tail = sum(math.comb(count, k) for k in range(min(wins, losses) + 1)) / 2 ** count
    return min(1.0, 2 * tail)


def paired_outcomes(rows, agents):
    if len(agents) != 2:
        return []
    first, second = agents
    by_key = {(row["source"], row["task"], row["attempt"], row["agent"]): row for row in rows}
    scored = ("passed", "failed")
    pairs = [
        (by_key[key[:3] + (first,)], row)
        for key, row in sorted(by_key.items())
        if key[3] == second and key[:3] + (first,) in by_key and row["outcome"] in scored and by_key[key[:3] + (first,)]["outcome"] in scored
    ]
    both = sum(base["outcome"] == "passed" and other["outcome"] == "passed" for base, other in pairs)
    neither = sum(base["outcome"] == "failed" and other["outcome"] == "failed" for base, other in pairs)
    only_second = sum(base["outcome"] == "failed" and other["outcome"] == "passed" for base, other in pairs)
    only_first = sum(base["outcome"] == "passed" and other["outcome"] == "failed" for base, other in pairs)
    return [
        "Paired outcomes (%d pairs both scored): both passed %d, both failed %d, only %s passed %d, only %s passed %d; exact two-sided sign test on the discordant pairs p = %.3f"
        % (len(pairs), both, neither, second, only_second, first, only_first, sign_test(only_second, only_first)),
        "Only %s passed: %s" % (first, ", ".join("%s.%d" % (base["task"], base["attempt"]) for base, other in pairs if base["outcome"] == "passed" and other["outcome"] == "failed") or "none"),
        "Only %s passed: %s" % (second, ", ".join("%s.%d" % (base["task"], base["attempt"]) for base, other in pairs if base["outcome"] == "failed" and other["outcome"] == "passed") or "none"),
    ]


def main():
    rows = []
    for out in sys.argv[1:]:
        rows += load(out)
    agents = sorted({row["agent"] for row in rows}, key=lambda agent: (agent != "nanocode", agent))
    models = sorted({row["model"] for row in rows})
    print("Model: " + ", ".join(models) + ". Prices: " + PRICES_SOURCE + ".")
    print()
    print("\n".join(summary(rows, agents)))
    print()
    print("\n".join(per_task(rows, agents)))
    print()
    for index, first in enumerate(agents):
        for other in agents[index + 1:]:
            pair = [first, other]
            print("\n".join(paired(rows, pair)))
            print()
            print("\n".join(differences(rows, pair)))
            print("\n".join(paired_outcomes(rows, pair)))
            print()
    for agent in agents:
        dropped = [row["id"] for row in rows if row["agent"] == agent and (row.get("meter") or {}).get("statuses", {}).get("0")]
        print("%s rows with a request the upstream connection dropped: %d %s" % (agent, len(dropped), " ".join(dropped)))
    errors = [row for row in rows if row["outcome"] == "errored"]
    for row in errors:
        print("errored: " + row["id"] + ": " + str(row.get("note")))


main()
