import glob
import json
import os
import sys

TURN_REQUESTS = 2
SHELL_CALLS = 1
METERED_KEY = "metered-by-host"


def main():
    directory, key = sys.argv[1], sys.argv[2]
    task = os.path.basename(os.path.dirname(directory)) + "/" + os.path.basename(directory)
    log = os.path.join(directory, "fake.jsonl")
    fake = [json.loads(line) for line in open(log)] if os.path.isfile(log) else []
    posts = [entry for entry in fake if entry["method"] == "POST"]
    gets = [entry for entry in fake if entry["method"] == "GET"]
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import fake_openai
    (result_path,) = glob.glob(os.path.join(directory, "run", "rows", "*", "result.json"))
    result = json.load(open(result_path))
    meter = result["meter"]
    records = [json.loads(line) for line in open(os.path.join(os.path.dirname(result_path), "requests.jsonl"))]
    records = [record for record in records if record["path"].split("?")[0].endswith("/responses")]
    expected = {
        "outcome": "passed",
        "fake posts": TURN_REQUESTS,
        "fake websocket upgrades reaching it": 0,
        "upstream authorization": ["Bearer " + key] * (TURN_REQUESTS + len(gets)),
        "metered key leaked upstream": False,
        "meter requests": TURN_REQUESTS,
        "meter ok_requests": TURN_REQUESTS,
        "meter tool_calls": SHELL_CALLS,
        "meter input_tokens": TURN_REQUESTS * (fake_openai.INPUT_TOKENS - fake_openai.CACHED_TOKENS),
        "meter cache_read_tokens": TURN_REQUESTS * fake_openai.CACHED_TOKENS,
        "meter output_tokens": TURN_REQUESTS * fake_openai.OUTPUT_TOKENS,
        "meter providers": ["openai"] * TURN_REQUESTS,
        "meter paths": ["/v1/responses"] * TURN_REQUESTS,
    }
    actual = {
        "outcome": result.get("outcome"),
        "fake posts": len(posts),
        "fake websocket upgrades reaching it": sum(1 for entry in fake if entry.get("upgrade")),
        "upstream authorization": [entry.get("authorization") for entry in posts + gets],
        "metered key leaked upstream": any(METERED_KEY in json.dumps(entry) for entry in fake),
        "meter requests": meter["requests"],
        "meter ok_requests": meter["ok_requests"],
        "meter tool_calls": meter["tool_calls"],
        "meter input_tokens": meter["input_tokens"],
        "meter cache_read_tokens": meter["cache_read_tokens"],
        "meter output_tokens": meter["output_tokens"],
        "meter providers": [record.get("provider") for record in records],
        "meter paths": [record["path"].split("?")[0] for record in records],
    }
    wrong = {name: (actual[name], expected[name]) for name in expected if actual[name] != expected[name]}
    for name, (got, want) in wrong.items():
        print("codex-drill: " + task + ": " + name + " is " + json.dumps(got) + ", expected " + json.dumps(want))
    if not wrong:
        print("codex-drill: " + task + ": passed through codex, the meter and the verifier; " + str(meter["requests"]) + " requests, " + str(meter["input_tokens"] + meter["cache_read_tokens"]) + " input tokens metered")
    return 1 if wrong else 0


sys.exit(main())
