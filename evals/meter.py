import http.client
import http.server
import json
import os
import socketserver
import ssl
import sys
import threading
import time
import urllib.parse

PROVIDERS = {
    "anthropic": {"origin": "https://api.anthropic.com", "key_env": "ANTHROPIC_API_KEY", "origin_env": "NANOEVAL_ANTHROPIC_UPSTREAM"},
    "openai": {"origin": "https://api.openai.com", "key_env": "OPENAI_API_KEY", "origin_env": "NANOEVAL_OPENAI_UPSTREAM"},
}
DEFAULT_PROVIDER = "anthropic"
TOOL_USE_TYPES = ("tool_use", "function_call", "custom_tool_call", "local_shell_call", "web_search_call")
WEBSOCKET_REFUSED = 426
UPSTREAM_TIMEOUT_SEC = 900
READ_CHUNK = 65536
DROPPED_REQUEST_HEADERS = {"host", "x-api-key", "authorization", "content-length", "transfer-encoding", "connection", "accept-encoding"}
DROPPED_RESPONSE_HEADERS = {"content-length", "transfer-encoding", "connection", "content-encoding"}


def upstream(origin):
    target = urllib.parse.urlsplit(origin)
    if target.scheme == "http":
        return http.client.HTTPConnection(target.hostname, target.port, timeout=UPSTREAM_TIMEOUT_SEC)
    context = ssl.create_default_context()
    proxy = urllib.parse.urlsplit(os.environ.get("HTTPS_PROXY") or os.environ.get("https_proxy") or "")
    if not proxy.hostname:
        return http.client.HTTPSConnection(target.hostname, target.port, timeout=UPSTREAM_TIMEOUT_SEC, context=context)
    connection = http.client.HTTPSConnection(proxy.hostname, proxy.port, timeout=UPSTREAM_TIMEOUT_SEC, context=context)
    connection.set_tunnel(target.hostname, target.port)
    return connection


def route(rest):
    head, _, tail = rest.partition("/")
    if head in PROVIDERS and head != DEFAULT_PROVIDER:
        return head, "/" + tail
    return DEFAULT_PROVIDER, "/" + rest


def credentials(provider, key):
    if provider == "openai":
        return {"authorization": "Bearer " + key}
    return {"x-api-key": key}


def read_body(handler):
    if handler.headers.get("transfer-encoding", "").lower() == "chunked":
        parts = []
        while True:
            size = int(handler.rfile.readline().strip().split(b";")[0], 16)
            if size == 0:
                handler.rfile.readline()
                return b"".join(parts)
            parts.append(handler.rfile.read(size))
            handler.rfile.readline()
    return handler.rfile.read(int(handler.headers.get("content-length", "0")))


def summarize_request(body):
    try:
        request = json.loads(body)
    except ValueError:
        return {}
    if not isinstance(request, dict):
        return {}
    system = request.get("system") or request.get("instructions")
    system_chars = len(system) if isinstance(system, str) else len(json.dumps(system)) if system else 0
    return {
        "model": request.get("model"),
        "stream": bool(request.get("stream")),
        "max_tokens": request.get("max_tokens") or request.get("max_output_tokens"),
        "thinking": request.get("thinking"),
        "effort": (request.get("output_config") or request.get("reasoning") or {}).get("effort"),
        "tools": [tool.get("name") or tool.get("type") for tool in request.get("tools") or []],
        "system_chars": system_chars,
        "messages": len(request.get("messages") or request.get("input") or []),
    }


class Assembler:
    def __init__(self):
        self.message = {"content": [], "usage": {}}
        self.buffer = b""

    def feed_stream(self, data):
        self.buffer += data
        while b"\n\n" in self.buffer:
            event, self.buffer = self.buffer.split(b"\n\n", 1)
            for line in event.split(b"\n"):
                if line.startswith(b"data:"):
                    self.event(json.loads(line[5:]))

    def event(self, event):
        kind = event.get("type")
        if kind == "message_start":
            message = event["message"]
            self.message.update({key: value for key, value in message.items() if key != "content"})
            self.message["usage"] = dict(message.get("usage") or {})
        elif kind == "content_block_start":
            block = dict(event["content_block"])
            if block.get("type") == "tool_use":
                block["input_json"] = ""
            self.message["content"].append(block)
        elif kind == "content_block_delta":
            block = self.message["content"][event["index"]]
            delta = event["delta"]
            if delta.get("type") == "text_delta":
                block["text"] = block.get("text", "") + delta["text"]
            elif delta.get("type") == "input_json_delta":
                block["input_json"] = block.get("input_json", "") + delta["partial_json"]
            elif delta.get("type") == "thinking_delta":
                block["thinking"] = block.get("thinking", "") + delta["thinking"]
        elif kind == "message_delta":
            self.message["stop_reason"] = (event.get("delta") or {}).get("stop_reason")
            self.message["usage"].update({key: value for key, value in (event.get("usage") or {}).items() if value is not None})
        elif kind == "error":
            self.message["error"] = event.get("error") or event
        elif kind == "response.output_item.done":
            self.message["content"].append(event["item"])
        elif kind in ("response.completed", "response.incomplete", "response.failed"):
            response = event["response"]
            self.message.update({key: value for key, value in response.items() if key != "output"})
            self.message["stop_reason"] = response.get("status")
            if response.get("error"):
                self.message["error"] = response["error"]


def usage_of(message):
    usage = message.get("usage") or {}
    if "input_tokens_details" in usage or "output_tokens_details" in usage:
        cached = (usage.get("input_tokens_details") or {}).get("cached_tokens") or 0
        return {
            "input_tokens": (usage.get("input_tokens") or 0) - cached,
            "output_tokens": usage.get("output_tokens") or 0,
            "cache_read_tokens": cached,
            "cache_write_tokens": 0,
            "cache_write_1h_tokens": 0,
            "reasoning_tokens": (usage.get("output_tokens_details") or {}).get("reasoning_tokens") or 0,
        }
    return {
        "input_tokens": usage.get("input_tokens") or 0,
        "output_tokens": usage.get("output_tokens") or 0,
        "cache_read_tokens": usage.get("cache_read_input_tokens") or 0,
        "cache_write_tokens": usage.get("cache_creation_input_tokens") or 0,
        "cache_write_1h_tokens": (usage.get("cache_creation") or {}).get("ephemeral_1h_input_tokens") or 0,
    }


class Meter(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    runs = "."
    keys = {}
    lock = threading.Lock()

    def log_message(self, format, *args):
        return

    def do_GET(self):
        if self.headers.get("upgrade", "").lower() == "websocket":
            self.send_error(WEBSOCKET_REFUSED, "the meter relays HTTP only")
            return
        self.forward(b"")

    def do_POST(self):
        self.forward(read_body(self))

    def forward(self, body):
        parts = self.path.split("/", 3)
        if len(parts) < 4 or parts[1] != "run" or parts[2] in ("", ".", ".."):
            self.send_error(404, "expected /run/<id>/<path>")
            return
        run = parts[2]
        provider, path = route(parts[3])
        directory = os.path.join(self.runs, run)
        if not os.path.isdir(directory):
            self.send_error(404, "unknown run " + run)
            return
        if provider not in self.keys:
            self.send_error(503, "the meter has no " + PROVIDERS[provider]["key_env"])
            return
        started = time.time()
        headers = {name: value for name, value in self.headers.items() if name.lower() not in DROPPED_REQUEST_HEADERS}
        headers.update(credentials(provider, self.keys[provider]))
        headers["accept-encoding"] = "identity"
        headers["content-length"] = str(len(body))
        connection = upstream(os.environ.get(PROVIDERS[provider]["origin_env"]) or PROVIDERS[provider]["origin"])
        record = {"run": run, "provider": provider, "path": path, "started": started, "request": summarize_request(body)}
        assembler = Assembler()
        raw = b""
        try:
            connection.request(self.command, path, body=body, headers=headers)
            response = connection.getresponse()
            streaming = "text/event-stream" in (response.getheader("content-type") or "")
            self.send_response(response.status)
            for name, value in response.getheaders():
                if name.lower() not in DROPPED_RESPONSE_HEADERS:
                    self.send_header(name, value)
            self.send_header("transfer-encoding", "chunked")
            self.end_headers()
            while True:
                data = response.read1(READ_CHUNK)
                if not data:
                    break
                if streaming:
                    assembler.feed_stream(data)
                else:
                    raw += data
                self.wfile.write(b"%x\r\n%s\r\n" % (len(data), data))
                self.wfile.flush()
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
            record["status"] = response.status
            if not streaming:
                try:
                    parsed = json.loads(raw)
                    if isinstance(parsed, dict) and parsed.get("type") == "message":
                        assembler.message = parsed
                    elif isinstance(parsed, dict) and parsed.get("object") == "response":
                        assembler.message = dict(parsed, content=parsed.get("output") or [])
                    else:
                        assembler.message["error"] = parsed
                except ValueError:
                    assembler.message["error"] = raw[:2000].decode(errors="replace")
        except (OSError, http.client.HTTPException) as error:
            record["status"] = 0
            assembler.message["error"] = repr(error)
        finally:
            connection.close()
        message = assembler.message
        record["ended"] = time.time()
        record["usage"] = usage_of(message)
        record["stop_reason"] = message.get("stop_reason")
        record["tool_uses"] = [block.get("name") or block.get("type") for block in message.get("content") or [] if block.get("type") in TOOL_USE_TYPES]
        if message.get("error"):
            record["error"] = message["error"]
        with self.lock:
            with open(os.path.join(directory, "requests.jsonl"), "a") as handle:
                handle.write(json.dumps(record) + "\n")
            with open(os.path.join(directory, "responses.jsonl"), "a") as handle:
                handle.write(json.dumps(message) + "\n")
            if body:
                with open(os.path.join(directory, "last-request.json"), "wb") as handle:
                    handle.write(body)


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


def main():
    if len(sys.argv) != 4:
        raise SystemExit("usage: meter.py ADDRESS PORT RUNS_DIR")
    keys = {provider: os.environ[spec["key_env"]] for provider, spec in PROVIDERS.items() if os.environ.get(spec["key_env"])}
    if not keys:
        raise SystemExit("meter.py needs " + " or ".join(spec["key_env"] for spec in PROVIDERS.values()))
    Meter.runs = os.path.abspath(sys.argv[3])
    Meter.keys = keys
    server = Server((sys.argv[1], int(sys.argv[2])), Meter)
    server.serve_forever()


main()
