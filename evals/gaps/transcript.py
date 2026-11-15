import json
import sys


def text_of(content):
    if isinstance(content, str):
        return content
    out = []
    for b in content:
        t = b.get("type")
        if t == "text":
            out.append(b["text"])
        elif t == "tool_result":
            out.append("RESULT " + text_of(b.get("content") or ""))
        elif t == "tool_use":
            out.append("CALL " + b["name"] + " " + json.dumps(b.get("input"))[:LIMIT])
    return "\n".join(out)


LIMIT = int(sys.argv[2]) if len(sys.argv) > 2 else 400

body = json.load(open(sys.argv[1] + "/last-request.json"))
for m in body["messages"]:
    t = text_of(m["content"])
    print("##", m["role"], t[:LIMIT].replace("\n", " | "))
for line in open(sys.argv[1] + "/responses.jsonl"):
    pass
last = json.loads(line)
print("## FINAL", text_of(last.get("content", []))[: LIMIT * 3].replace("\n", " | "))
