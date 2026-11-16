import glob
import json
import os
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.abspath(__file__))
COPIED = ("result.json", "requests.jsonl", "responses.jsonl", "journal/agent.stderr", "journal/agent.stdout")
LABEL = "hl-agent-luna"


def snapshot(rows, snap):
    shutil.rmtree(snap, ignore_errors=True)
    for path in sorted(glob.glob(os.path.join(rows, "*", "result.json"))):
        source = os.path.dirname(path)
        target = os.path.join(snap, "rows", os.path.basename(source))
        os.makedirs(os.path.join(target, "journal"))
        for name in COPIED:
            if os.path.isfile(os.path.join(source, name)):
                shutil.copy(os.path.join(source, name), os.path.join(target, name))


def ratio(numerator, denominator):
    return "%d/%d = %.0f%%" % (numerator, denominator, 100 * numerator / denominator) if denominator else "%d/0" % numerator


def escalation(results, escalate):
    scored = [r for r in results if r["outcome"] in ("passed", "failed")]
    flagged = [r for r in scored if escalate(r)]
    failed = [r for r in scored if r["outcome"] == "failed"]
    hits = [r for r in flagged if r["outcome"] == "failed"]
    return "flagged %d of %d; precision %s; recall %s" % (len(flagged), len(scored), ratio(len(hits), len(flagged)), ratio(len(hits), len(failed)))


def self_check_lines(snap, design):
    results = [json.load(open(path)) for path in sorted(glob.glob(os.path.join(snap, "rows", "*", "result.json")))]
    counts = {}
    for r in results:
        key = (r.get("self_check"), r["outcome"])
        counts[key] = counts.get(key, 0) + 1
    lines = ["| self_check | passed | failed | errored |", "| --- | ---: | ---: | ---: |"]
    for verdict in ("pass", "fail", "unknown", None):
        lines.append("| %s | %d | %d | %d |" % (verdict, counts.get((verdict, "passed"), 0), counts.get((verdict, "failed"), 0), counts.get((verdict, "errored"), 0)))
    lines.append("")
    groups = [("all rows", results)]
    if design:
        groups += [("rows the cue list was written from", [r for r in results if r["id"] in design]), ("later rows", [r for r in results if r["id"] not in design])]
    for name, group in groups:
        lines.append("- %s, escalate on fail: %s" % (name, escalation(group, lambda r: r.get("self_check") == "fail")))
        lines.append("- %s, escalate on fail or unknown: %s" % (name, escalation(group, lambda r: r.get("self_check") != "pass")))
    return lines


def main():
    rows, reference, snap = sys.argv[1:4]
    design = set(open(sys.argv[4]).read().split()) if len(sys.argv) > 4 else set()
    snapshot(rows, snap)
    subprocess.run([sys.executable, os.path.join(ROOT, "selfcheck.py"), os.path.join(snap, "rows")], check=True, capture_output=True)
    report = subprocess.run([sys.executable, os.path.join(ROOT, "report.py"), reference, snap + "=" + LABEL], check=True, capture_output=True, text=True)
    print(report.stdout)
    print("\n".join(self_check_lines(snap, design)))


main()
