import json
import os
import sys

from meter import usage_of
from run import meter_totals


def remeter(row):
    requests_path = os.path.join(row, "requests.jsonl")
    responses_path = os.path.join(row, "responses.jsonl")
    if not os.path.isfile(requests_path) or not os.path.isfile(responses_path):
        return False
    records = [json.loads(line) for line in open(requests_path)]
    messages = [json.loads(line) for line in open(responses_path)]
    if len(records) != len(messages):
        raise SystemExit(row + ": " + str(len(records)) + " requests but " + str(len(messages)) + " responses")
    for record, message in zip(records, messages):
        record["usage"] = usage_of(message)
    with open(requests_path, "w") as handle:
        handle.writelines(json.dumps(record) + "\n" for record in records)
    result_path = os.path.join(row, "result.json")
    if os.path.isfile(result_path):
        result = json.load(open(result_path))
        result["meter"] = meter_totals(row)
        with open(result_path, "w") as handle:
            json.dump(result, handle, indent=1)
    return True


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: remeter.py RUN_DIR...")
    for directory in sys.argv[1:]:
        rows = os.path.join(directory, "rows")
        done = sum(remeter(os.path.join(rows, name)) for name in sorted(os.listdir(rows)))
        print(directory + ": " + str(done) + " rows remetered")


if __name__ == "__main__":
    main()
