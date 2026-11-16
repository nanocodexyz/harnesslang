import glob
import json
import os
import re
import sys

REPORT_TAIL_CHARS = 4000
DOUBT = re.compile(
    r"cannot|can't|could not|couldn't|unable|unverified|not verified|not been verified|blocked|not yet"
    r"|does not match|doesn't match|did not match|mismatch|imperfect|depends on|partially|incomplete|timed out"
    r"|\bfail(s|ed|ing|ure|ures)?\b",
    re.IGNORECASE,
)


def final_report(row_dir):
    path = os.path.join(row_dir, "journal", "agent.stdout")
    if not os.path.isfile(path):
        return ""
    with open(path, errors="replace") as handle:
        return handle.read()[-REPORT_TAIL_CHARS:].strip()


def self_check(row_dir, result):
    agent = result.get("agent_result") or {}
    if agent.get("exit") != 0 or agent.get("timed_out"):
        return "unknown", None
    report = final_report(row_dir)
    if not report:
        return "unknown", None
    doubt = DOUBT.search(report)
    if doubt:
        return "fail", doubt.group(0)
    return "pass", None


def annotate(rows_dir):
    counts = {}
    for path in sorted(glob.glob(os.path.join(rows_dir, "*", "result.json"))):
        with open(path) as handle:
            result = json.load(handle)
        if "outcome" not in result:
            continue
        verdict, cue = self_check(os.path.dirname(path), result)
        result["self_check"] = verdict
        result["self_check_cue"] = cue
        with open(path + ".tmp", "w") as handle:
            json.dump(result, handle, indent=1)
        os.replace(path + ".tmp", path)
        counts[verdict] = counts.get(verdict, 0) + 1
    return counts


def main():
    for rows_dir in sys.argv[1:]:
        print(rows_dir, json.dumps(annotate(rows_dir), sort_keys=True))


main()
