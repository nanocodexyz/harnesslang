import http.client
import ipaddress
import http.server
import json
import os
import socketserver
import ssl
import sys
import threading
import time
import urllib.parse
import uuid

import checkpoint
import checkpoint_meter

PROVIDERS = {
    "anthropic": {"origin": "https://api.anthropic.com", "key_env": "ANTHROPIC_API_KEY", "origin_env": "NANOEVAL_ANTHROPIC_UPSTREAM"},
    "openai": {"origin": "https://api.openai.com", "key_env": "OPENAI_API_KEY", "origin_env": "NANOEVAL_OPENAI_UPSTREAM"},
    "chatgpt": {"origin": "https://chatgpt.com", "key_env": "NANOEVAL_CHATGPT_LOGIN", "origin_env": "NANOEVAL_CHATGPT_UPSTREAM"},
}
CHATGPT_PREFIX = "/backend-api/codex"
CHATGPT_MODELS_PATH = CHATGPT_PREFIX + "/models"
CHATGPT_CLIENT_VERSION = "0.169.0"
CHATGPT_LITE_HEADER = "x-openai-internal-codex-responses-lite"
CHATGPT_LITE_CONTEXT = "all_turns"
CHATGPT_HALT_STATUSES = (401, 403, 429)
CHATGPT_HALT_FILE = "chatgpt-halted.json"
LOGIN_PRIVATE_MODE = 0o077
SSE_PREFIXES = (b"event:", b"data:")
CLIENTS_ENV = "NANOEVAL_METER_CLIENTS"
DEFAULT_PROVIDER = "anthropic"
TOOL_USE_TYPES = ("tool_use", "function_call", "custom_tool_call", "local_shell_call", "web_search_call")
WEBSOCKET_REFUSED = 426
UPSTREAM_TIMEOUT_SEC = 900
READ_CHUNK = 65536
DROPPED_REQUEST_HEADERS = {"host", "x-api-key", "authorization", "chatgpt-account-id", "content-length", "transfer-encoding", "connection", "accept-encoding"}
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
    if head == "chatgpt":
        path = "/" + tail
        if path.startswith("/v1/"):
            path = path[len("/v1"):]
        return head, CHATGPT_PREFIX + path
    if head in PROVIDERS and head != DEFAULT_PROVIDER:
        return head, "/" + tail
    return DEFAULT_PROVIDER, "/" + rest


def platform_models(path):
    target = urllib.parse.urlsplit(path)
    if target.path != CHATGPT_MODELS_PATH:
        return path, False
    query = urllib.parse.parse_qs(target.query)
    if "client_version" in query:
        return path, False
    query["client_version"] = [CHATGPT_CLIENT_VERSION]
    return target.path + "?" + urllib.parse.urlencode(query, doseq=True), True


def platform_model_list(raw):
    listed = json.loads(raw).get("models") or []
    return json.dumps({"object": "list", "data": [{"id": model["slug"], "object": "model", "owned_by": "openai", "context_window": model.get("context_window")} for model in listed if model.get("slug")]}).encode()


def lite_context(body):
    try:
        request = json.loads(body)
    except ValueError:
        return False
    return isinstance(request, dict) and (request.get("reasoning") or {}).get("context") == CHATGPT_LITE_CONTEXT


def chatgpt_login(path):
    if os.stat(path).st_mode & LOGIN_PRIVATE_MODE:
        raise SystemExit(path + " must not be readable by group or other users")
    with open(path) as handle:
        login = json.load(handle)
    tokens = login.get("tokens") or {}
    if login.get("auth_mode") != "chatgpt" or not isinstance(tokens.get("access_token"), str) or not isinstance(tokens.get("account_id"), str):
        raise SystemExit(path + " holds no ChatGPT plan login")
    return tokens


def credentials(provider, key):
    if provider == "chatgpt":
        tokens = chatgpt_login(key)
        return {"authorization": "Bearer " + tokens["access_token"], "chatgpt-account-id": tokens["account_id"]}
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
        self.complete = False

    def feed_stream(self, data):
        self.buffer += data
        while b"\n\n" in self.buffer:
            event, self.buffer = self.buffer.split(b"\n\n", 1)
            for line in event.split(b"\n"):
                if line.startswith(b"data:"):
                    self.event(json.loads(line[5:]))

    def event(self, event):
        kind = event.get("type")
        if kind == "message_stop":
            self.complete = True
        elif kind == "message_start":
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
            self.complete = True
            response = event["response"]
            self.message.update({key: value for key, value in response.items() if key != "output"})
            self.message["stop_reason"] = response.get("status")
            if response.get("error"):
                self.message["error"] = response["error"]


def usage_of(message):
    usage = message.get("usage") or {}
    anthropic = "cache_read_input_tokens" in usage or "cache_creation_input_tokens" in usage
    if not anthropic and ("input_tokens_details" in usage or "output_tokens_details" in usage):
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
        "reasoning_tokens": (usage.get("output_tokens_details") or {}).get("thinking_tokens") or 0,
    }


class Meter(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    runs = "."
    keys = {}
    lock = threading.Lock()
    halted = None
    clients = None

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
        if self.clients is not None and not any(ipaddress.ip_address(self.client_address[0]) in network for network in self.clients):
            self.send_error(403, "the meter serves only " + os.environ[CLIENTS_ENV])
            return
        parts = self.path.split("/", 3)
        if len(parts) < 4 or parts[1] != "run" or parts[2] in ("", ".", ".."):
            self.send_error(404, "expected /run/<id>/<path>")
            return
        run = parts[2]
        provider, path = route(parts[3])
        translated = False
        if provider == "chatgpt" and self.command == "GET":
            path, translated = platform_models(path)
        directory = os.path.join(self.runs, run)
        if not os.path.isdir(directory):
            self.send_error(404, "unknown run " + run)
            return
        if provider not in self.keys:
            self.send_error(503, "the meter has no " + PROVIDERS[provider]["key_env"])
            return
        if provider == "chatgpt" and Meter.halted is not None:
            self.send_error(503, "the meter stopped the ChatGPT route after status " + str(Meter.halted))
            return
        request_id = None
        if path.split("?")[0].endswith(checkpoint_meter.MODEL_ENDPOINTS):
            try:
                request_id = checkpoint_meter.begin(directory, self.incarnation)
            except checkpoint.ResumeError as error:
                self.send_error(503, str(error))
                return
        started = time.time()
        headers = {name: value for name, value in self.headers.items() if name.lower() not in DROPPED_REQUEST_HEADERS}
        if provider == "chatgpt" and not lite_context(body):
            headers = {name: value for name, value in headers.items() if name.lower() != CHATGPT_LITE_HEADER}
        headers.update(credentials(provider, self.keys[provider]))
        headers["accept-encoding"] = "identity"
        headers["content-length"] = str(len(body))
        connection = upstream(os.environ.get(PROVIDERS[provider]["origin_env"]) or PROVIDERS[provider]["origin"])
        record = {"run": run, "provider": provider, "path": path, "started": started, "request": summarize_request(body)}
        if request_id:
            record.update(request_id=request_id, meter_incarnation=self.incarnation)
        assembler = Assembler()
        raw = b""
        streaming = False
        try:
            connection.request(self.command, path, body=body, headers=headers)
            response = connection.getresponse()
            if translated and response.status == 200:
                assert request_id is None
                self.relay_translated(response)
                record["status"] = response.status
                return
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
            record["status"] = response.status
            if provider == "chatgpt" and response.status in CHATGPT_HALT_STATUSES:
                self.halt(response.status, run)
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
                    if raw.lstrip().startswith(SSE_PREFIXES):
                        streaming = True
                        assembler.feed_stream(raw + b"\n\n")
                    else:
                        assembler.message["error"] = raw[:2000].decode(errors="replace")
        except (OSError, http.client.HTTPException) as error:
            record["status"] = 0
            assembler.message["error"] = repr(error)
        finally:
            connection.close()
        message = assembler.message
        record["ended"] = time.time()
        record["usage_known"] = record.get("status") == 200 and (not streaming or assembler.complete) and all(type((message.get("usage") or {}).get(key)) is int and message["usage"][key] >= 0 for key in ("input_tokens", "output_tokens"))
        record["usage"] = usage_of(message)
        record["stop_reason"] = message.get("stop_reason")
        record["tool_uses"] = [block.get("name") or block.get("type") for block in message.get("content") or [] if block.get("type") in TOOL_USE_TYPES]
        if message.get("error"):
            record["error"] = message["error"]
        if request_id:
            checkpoint_meter.finish(directory, request_id, record)
        with self.lock:
            with open(os.path.join(directory, "requests.jsonl"), "a") as handle:
                handle.write(json.dumps(record) + "\n")
            with open(os.path.join(directory, "responses.jsonl"), "a") as handle:
                handle.write(json.dumps(message) + "\n")
            if body:
                with open(os.path.join(directory, "last-request.json"), "wb") as handle:
                    handle.write(body)
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()


    def relay_translated(self, response):
        data = platform_model_list(response.read())
        self.send_response(response.status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
        self.wfile.flush()

    def halt(self, status, run):
        with self.lock:
            Meter.halted = status
            with open(os.path.join(self.runs, CHATGPT_HALT_FILE), "w") as handle:
                json.dump({"status": status, "run": run, "at": time.time()}, handle)


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


def main():
    if len(sys.argv) != 4:
        raise SystemExit("usage: meter.py ADDRESS PORT RUNS_DIR")
    keys = {provider: os.environ[spec["key_env"]] for provider, spec in PROVIDERS.items() if os.environ.get(spec["key_env"])}
    if "chatgpt" in keys:
        chatgpt_login(keys["chatgpt"])
    if not keys:
        raise SystemExit("meter.py needs " + " or ".join(spec["key_env"] for spec in PROVIDERS.values()))
    Meter.incarnation = getattr(Meter, "incarnation", None) or uuid.uuid4().hex
    Meter.runs = os.path.abspath(sys.argv[3])
    Meter.keys = keys
    if os.environ.get(CLIENTS_ENV):
        Meter.clients = [ipaddress.ip_network(network) for network in os.environ[CLIENTS_ENV].split(",")]
    server = Server((sys.argv[1], int(sys.argv[2])), Meter)
    server.serve_forever()


if __name__ == "__main__":
    main()
