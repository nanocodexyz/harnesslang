import http.server
import json
import os
import socketserver
import sys
import threading

SHELL_TOOLS = ("exec_command", "shell_command", "shell")
INPUT_TOKENS = 1000
CACHED_TOKENS = 600
OUTPUT_TOKENS = 50
REASONING_TOKENS = 20
FINAL_TEXT = "done"


def shell_call(tools, script):
    names = [tool.get("name") for tool in tools if tool.get("type") == "function"]
    for name in SHELL_TOOLS:
        if name in names:
            if name == "exec_command":
                return name, {"cmd": script}
            if name == "shell_command":
                return name, {"command": script}
            return name, {"command": ["sh", "-c", script]}
    raise ValueError("no shell tool among " + ",".join(str(name) for name in names))


def answered(request):
    return any(isinstance(item, dict) and item.get("type", "").endswith("_output") for item in request.get("input") or [])


def events(request, script, number):
    response = "resp_" + str(number)
    usage = {
        "input_tokens": INPUT_TOKENS,
        "input_tokens_details": {"cached_tokens": CACHED_TOKENS},
        "output_tokens": OUTPUT_TOKENS,
        "output_tokens_details": {"reasoning_tokens": REASONING_TOKENS},
        "total_tokens": INPUT_TOKENS + OUTPUT_TOKENS,
    }
    if answered(request):
        item = {"type": "message", "role": "assistant", "id": "msg_" + str(number), "content": [{"type": "output_text", "text": FINAL_TEXT}]}
    else:
        name, arguments = shell_call(request.get("tools") or [], script)
        item = {"type": "function_call", "id": "fc_" + str(number), "call_id": "call_" + str(number), "name": name, "arguments": json.dumps(arguments)}
    return [
        {"type": "response.created", "response": {"id": response}},
        {"type": "response.output_item.done", "item": item},
        {"type": "response.completed", "response": {"id": response, "usage": usage}},
    ]


class Fake(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    script = "true"
    model = None
    log = None
    lock = threading.Lock()
    count = 0

    def log_message(self, format, *args):
        return

    def record(self, entry):
        with self.lock:
            with open(self.log, "a") as handle:
                handle.write(json.dumps(entry) + "\n")

    def reply(self, status, kind, body):
        self.send_response(status)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.record({"method": "GET", "path": self.path, "authorization": self.headers.get("authorization"), "upgrade": self.headers.get("upgrade")})
        if self.path.split("?")[0].endswith("/models") and not self.headers.get("upgrade"):
            self.reply(200, "application/json", json.dumps({"object": "list", "data": [{"id": self.model, "object": "model"}]}).encode())
            return
        self.reply(404, "application/json", b'{"error":{"message":"not served by the fake"}}')

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", "0")))
        with self.lock:
            Fake.count += 1
            number = Fake.count
        try:
            request = json.loads(body)
        except ValueError:
            self.record({"method": "POST", "path": self.path, "encoding": self.headers.get("content-encoding"), "unparsed": len(body)})
            self.reply(400, "application/json", b'{"error":{"message":"the fake reads JSON bodies only"}}')
            return
        self.record({"method": "POST", "path": self.path, "authorization": self.headers.get("authorization"), "model": request.get("model"), "stream": request.get("stream"), "tools": [tool.get("name") for tool in request.get("tools") or []], "input_items": len(request.get("input") or [])})
        if not self.path.split("?")[0].endswith("/responses"):
            self.reply(404, "application/json", b'{"error":{"message":"not served by the fake"}}')
            return
        stream = "".join("event: " + event["type"] + "\ndata: " + json.dumps(event) + "\n\n" for event in events(request, self.script, number))
        self.reply(200, "text/event-stream", stream.encode())


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


def main():
    if len(sys.argv) != 6:
        raise SystemExit("usage: fake_openai.py ADDRESS PORT SCRIPT_FILE LOG MODEL")
    Fake.script = open(sys.argv[3]).read()
    Fake.log = os.path.abspath(sys.argv[4])
    Fake.model = sys.argv[5]
    Server((sys.argv[1], int(sys.argv[2])), Fake).serve_forever()


if __name__ == "__main__":
    main()
