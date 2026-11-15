import json
import os
import re
import statistics
import sys

VERIFY = re.compile(
    r"\b(pytest|tests?|verify|diff|cmp|python3? -c|python3? [\w/.-]+\.py|ls -la?|cat|head|stat|wc|sqlite3|curl|jq|md5sum|sha256sum)\b"
)
SLEEP = re.compile(r"\bsleep\s+\d")
DETACH = re.compile(r"(&\s*$|&\s*\n|\bnohup\b|\bsetsid\b|\bdisown\b)", re.M)


def load_jsonl(path):
    out = []
    if not os.path.exists(path):
        return out
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    return out


def tool_input(block):
    raw = block.get("input_json")
    if raw:
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return {}
    return block.get("input") or {}


def command_of(name, args):
    if name == "Bash":
        return args.get("command", "")
    if name == "exec":
        return args.get("code", "") or args.get("command", "")
    return ""


def row_stats(row_dir):
    result = json.load(open(os.path.join(row_dir, "result.json")))
    requests = load_jsonl(os.path.join(row_dir, "requests.jsonl"))
    responses = load_jsonl(os.path.join(row_dir, "responses.jsonl"))
    per_reply = []
    names = {}
    timeouts = []
    background = 0
    detached = 0
    sleeps = 0
    stops = {}
    commands = []
    for resp in responses:
        stop = resp.get("stop_reason")
        stops[str(stop)] = stops.get(str(stop), 0) + 1
        uses = [b for b in resp.get("content", []) if b.get("type") == "tool_use"]
        per_reply.append(len(uses))
        for b in uses:
            name = b.get("name")
            names[name] = names.get(name, 0) + 1
            args = tool_input(b)
            if "timeout" in args:
                timeouts.append(args["timeout"])
            if args.get("run_in_background"):
                background += 1
            cmd = command_of(name, args)
            if cmd:
                commands.append(cmd)
                if SLEEP.search(cmd):
                    sleeps += 1
                if DETACH.search(cmd):
                    detached += 1
    statuses = {}
    gaps = []
    model_s = 0.0
    for i, req in enumerate(requests):
        key = str(req.get("status"))
        statuses[key] = statuses.get(key, 0) + 1
        if req.get("ended") and req.get("started"):
            model_s += req["ended"] - req["started"]
        if i > 0 and requests[i - 1].get("ended") and req.get("started"):
            gaps.append(req["started"] - requests[i - 1]["ended"])
    messages = [r.get("request", {}).get("messages", 0) for r in requests]
    shrinks = sum(1 for a, b in zip(messages, messages[1:]) if b < a)
    last = responses[-1] if responses else {}
    final_text = "".join(
        b.get("text", "") for b in last.get("content", []) if b.get("type") == "text"
    )
    tail = commands[-3:]
    usage = [r.get("usage") or {} for r in requests]
    inp = sum(u.get("input_tokens", 0) or 0 for u in usage)
    cr = sum(u.get("cache_read_tokens", 0) or 0 for u in usage)
    cw = sum(u.get("cache_write_tokens", 0) or 0 for u in usage)
    first = requests[0].get("request", {}) if requests else {}
    agent = result.get("agent_result") or {}
    return {
        "row": os.path.basename(row_dir),
        "dir": row_dir,
        "task": result["task"],
        "agent": result["agent"],
        "outcome": result["outcome"],
        "wall_s": round((agent.get("wall_ms") or 0) / 1000),
        "timed_out": bool(agent.get("timed_out")),
        "requests": len(requests),
        "statuses": statuses,
        "replies": len(responses),
        "tool_calls": sum(per_reply),
        "multi_tool_replies": sum(1 for n in per_reply if n > 1),
        "max_tools_in_reply": max(per_reply) if per_reply else 0,
        "tool_names": names,
        "timeouts": timeouts,
        "background": background,
        "detached": detached,
        "sleeps": sleeps,
        "stops": stops,
        "final_stop": str(last.get("stop_reason")),
        "final_text_chars": len(final_text),
        "verified_at_end": any(VERIFY.search(c) for c in tail),
        "last_commands": [c[:200] for c in tail],
        "model_s": round(model_s),
        "tool_gap_s": round(sum(g for g in gaps if g > 0)),
        "max_tool_gap_s": round(max(gaps)) if gaps else 0,
        "message_shrinks": shrinks,
        "max_messages": max(messages) if messages else 0,
        "cache_read_share": round(cr / (inp + cr + cw), 3) if inp + cr + cw else None,
        "system_chars": first.get("system_chars"),
        "max_tokens": first.get("max_tokens"),
    }


def all_rows(dirs):
    out = []
    for d in dirs:
        base = os.path.join(d, "rows")
        for name in sorted(os.listdir(base)):
            path = os.path.join(base, name)
            if os.path.exists(os.path.join(path, "result.json")):
                out.append(row_stats(path))
    return out


def mean(xs):
    return round(statistics.mean(xs), 2) if xs else None


def summarize(rows):
    by_agent = {}
    for r in rows:
        by_agent.setdefault(r["agent"], []).append(r)
    summary = {}
    for agent, rs in sorted(by_agent.items()):
        replies = sum(r["replies"] for r in rs)
        summary[agent] = {
            "rows": len(rs),
            "passed": sum(1 for r in rs if r["outcome"] == "passed"),
            "timed_out": sum(1 for r in rs if r["timed_out"]),
            "multi_tool_reply_share": round(sum(r["multi_tool_replies"] for r in rs) / replies, 3) if replies else None,
            "tool_calls_per_reply": round(sum(r["tool_calls"] for r in rs) / replies, 2) if replies else None,
            "timeout_args_per_row": mean([len(r["timeouts"]) for r in rs]),
            "run_in_background_per_row": mean([r["background"] for r in rs]),
            "detached_per_row": mean([r["detached"] for r in rs]),
            "sleeps_per_row": mean([r["sleeps"] for r in rs]),
            "model_s_per_row": mean([r["model_s"] for r in rs]),
            "tool_gap_s_per_row": mean([r["tool_gap_s"] for r in rs]),
            "verified_at_end_share": round(sum(1 for r in rs if r["verified_at_end"]) / len(rs), 3),
            "final_stop": {k: sum(1 for r in rs if r["final_stop"] == k) for k in sorted({r["final_stop"] for r in rs})},
            "message_shrink_rows": sum(1 for r in rs if r["message_shrinks"]),
            "cache_read_share": mean([r["cache_read_share"] for r in rs if r["cache_read_share"] is not None]),
            "non_200": sum(v for r in rs for k, v in r["statuses"].items() if k != "200"),
        }
    return summary


def main():
    rows = all_rows(sys.argv[1:])
    json.dump({"summary": summarize(rows), "rows": rows}, sys.stdout, indent=1)


if __name__ == "__main__":
    main()
