#[path = "../json.rs"]
#[allow(dead_code)]
mod json;

use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use json::Value;

const CACHE_BREAKPOINTS_MAX: usize = 4;
const HEADER_LINES_MAX: usize = 128;
const USAGE_INPUT_PER_MESSAGE: u64 = 100;
const USAGE_OUTPUT: u64 = 20;
const USAGE_CACHE_READ: u64 = 3_000;
const HTTP_TOO_MANY: u16 = 429;
const CHILD_TOOL: &str = "submit_result";
const SPAWN_TOOL: &str = "spawn_agent";
const SPAWNED: &str = "$spawned";

#[derive(Clone)]
struct Scripts {
    parent: Vec<Value>,
    child: Vec<Value>,
}

fn steps_of(path: &str) -> Vec<Value> {
    let script = Value::decode(&std::fs::read_to_string(path).expect("read the script"))
        .expect("the script is JSON");
    script
        .as_array()
        .expect("the script is a list of steps")
        .to_vec()
}

fn is_child(body: &Value) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some(CHILD_TOOL))
}

fn spawned(messages: &[Value]) -> Option<String> {
    let blocks = || {
        messages.iter().flat_map(|message| {
            message
                .get("content")
                .and_then(Value::as_array)
                .unwrap_or(&[])
                .iter()
        })
    };
    let id = blocks()
        .filter(|block| block.get("name").and_then(Value::as_str) == Some(SPAWN_TOOL))
        .filter_map(|block| block.get("id").and_then(Value::as_str))
        .next_back()?;
    blocks()
        .find(|block| block.get("tool_use_id").and_then(Value::as_str) == Some(id))
        .and_then(|block| block.get("content").and_then(Value::as_str))
        .map(str::to_owned)
}

fn filled(input: &Value, messages: &[Value]) -> Value {
    match input {
        Value::String(text) if text == SPAWNED => {
            Value::String(spawned(messages).unwrap_or_default())
        }
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), filled(value, messages)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (port_file, script, delay_ms, child) = match args.as_slice() {
        [port_file, script, delay_ms] => (port_file, script, delay_ms, None),
        [port_file, script, delay_ms, child] => (port_file, script, delay_ms, Some(child)),
        _ => {
            eprintln!("usage: mock-messages PORT_FILE SCRIPT_JSON DELAY_MS [CHILD_SCRIPT_JSON]");
            std::process::exit(2);
        }
    };
    let steps = Scripts {
        parent: steps_of(script),
        child: child.map(|child| steps_of(child)).unwrap_or_default(),
    };
    let delay = Duration::from_millis(delay_ms.parse().expect("DELAY_MS is a number"));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
    let port = listener.local_addr().expect("a bound address").port();
    std::fs::write(format!("{port_file}.new"), port.to_string()).expect("write the port");
    std::fs::rename(format!("{port_file}.new"), port_file).expect("publish the port");
    let throttled = Arc::new(Mutex::new(BTreeSet::new()));
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let steps = steps.clone();
        let throttled = Arc::clone(&throttled);
        std::thread::spawn(move || serve(stream, &steps, delay, &throttled));
    }
}

fn throttle(
    steps: &[Value],
    step: (bool, usize),
    throttled: &Mutex<BTreeSet<(bool, usize)>>,
) -> Option<u64> {
    let seconds = steps
        .get(step.1)?
        .get("throttle")?
        .as_u64()
        .filter(|seconds| *seconds > 0)?;
    let mut seen = throttled
        .lock()
        .expect("the throttle set is never poisoned");
    seen.insert(step).then_some(seconds)
}

fn streamed(body: &str) -> bool {
    Value::decode(body)
        .ok()
        .and_then(|body| body.get("stream").cloned())
        .is_some_and(|stream| matches!(stream, Value::Bool(true)))
}

fn sse(kind: &str, data: Value) -> String {
    let mut data = data;
    data.insert("type", Value::text(kind));
    format!("event: {kind}\ndata: {}\n\n", data.encode())
}

fn halves(text: &str) -> (String, String) {
    let mut middle = text.len() / 2;
    while !text.is_char_boundary(middle) {
        middle += 1;
    }
    (text[..middle].to_owned(), text[middle..].to_owned())
}

fn events(message: &Value) -> Vec<String> {
    let mut head = message.clone();
    head.insert("content", Value::Array(Vec::new()));
    head.insert("stop_reason", Value::Null);
    let mut out = vec![sse("message_start", Value::object([("message", head)]))];
    let blocks = message
        .get("content")
        .and_then(Value::as_array)
        .unwrap_or(&[]);
    for (index, block) in blocks.iter().enumerate() {
        let at = Value::number(index as u64);
        let (start, delta_kind, key, text) = match block.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                let mut start = block.clone();
                start.insert("input", Value::object([]));
                let input = block.get("input").map(Value::encode).unwrap_or_default();
                (start, "input_json_delta", "partial_json", input)
            }
            _ => {
                let mut start = block.clone();
                start.insert("text", Value::text(""));
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                (start, "text_delta", "text", text)
            }
        };
        out.push(sse(
            "content_block_start",
            Value::object([("index", at.clone()), ("content_block", start)]),
        ));
        let (first, second) = halves(&text);
        for part in [first, second] {
            out.push(sse(
                "content_block_delta",
                Value::object([
                    ("index", at.clone()),
                    (
                        "delta",
                        Value::object([
                            ("type", Value::text(delta_kind)),
                            (key, Value::String(part)),
                        ]),
                    ),
                ]),
            ));
        }
        out.push(sse("content_block_stop", Value::object([("index", at)])));
    }
    let stop = message.get("stop_reason").cloned().unwrap_or(Value::Null);
    let usage = message.get("usage").cloned().unwrap_or(Value::object([]));
    out.push(sse(
        "message_delta",
        Value::object([
            ("delta", Value::object([("stop_reason", stop)])),
            ("usage", usage),
        ]),
    ));
    out.push(sse("message_stop", Value::object([])));
    out
}

fn serve_stream(mut stream: TcpStream, message: &Value, delay: Duration) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let events = events(message);
    let pause = delay / u32::try_from(events.len()).expect("a scripted message has few events");
    for event in events {
        std::thread::sleep(pause);
        if stream.write_all(event.as_bytes()).is_err() {
            return;
        }
    }
}

static REQUEST_LOG: Mutex<()> = Mutex::new(());

fn log_request(body: &str) {
    let Some(path) = std::env::var_os("MOCK_REQUEST_LOG") else {
        return;
    };
    let _held = REQUEST_LOG.lock().expect("the request log lock is never poisoned");
    let line = Value::decode(body).map(|value| value.encode()).unwrap_or_else(|_| body.to_owned());
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn serve(
    mut stream: TcpStream,
    scripts: &Scripts,
    delay: Duration,
    throttled: &Mutex<BTreeSet<(bool, usize)>>,
) {
    let Some((headers, body)) = read_request(&stream) else {
        return;
    };
    log_request(&body);
    let child = Value::decode(&body).is_ok_and(|body| is_child(&body));
    let steps = if child {
        &scripts.child
    } else {
        &scripts.parent
    };
    let step = Value::decode(&body).ok().and_then(|body| check(&body).ok());
    if let Some(seconds) = step.and_then(|step| throttle(steps, (child, step), throttled)) {
        let text = Value::object([
            ("type", Value::text("error")),
            (
                "error",
                Value::object([
                    ("type", Value::text("rate_limit_error")),
                    ("message", Value::text("scripted throttle")),
                ]),
            ),
        ])
        .encode();
        let response = format!(
            "HTTP/1.1 {HTTP_TOO_MANY} Too Many Requests\r\nContent-Type: application/json\r\nretry-after: {seconds}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        let _ = stream.write_all(response.as_bytes());
        return;
    }
    if streamed(&body)
        && let Ok(message) = answer(&headers, &body, steps)
    {
        serve_stream(stream, &message, delay);
        return;
    }
    let (status, reply) = match answer(&headers, &body, steps) {
        Ok(reply) => (200, reply),
        Err(error) => {
            eprintln!("mock-messages: 400 {error}");
            (
                400,
                Value::object([
                    ("type", Value::text("error")),
                    (
                        "error",
                        Value::object([
                            ("type", Value::text("invalid_request_error")),
                            ("message", Value::text(&error)),
                        ]),
                    ),
                ]),
            )
        }
    };
    std::thread::sleep(delay);
    let text = reply.encode();
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let (first, second) = text.split_at(text.len() / 2);
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{first}\r\n{:x}\r\n{second}\r\n0\r\n\r\n",
        first.len(),
        second.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn read_request(stream: &TcpStream) -> Option<(Vec<String>, String)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut headers = Vec::new();
    for _ in 0..HEADER_LINES_MAX {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end().to_owned();
        if line.is_empty() {
            break;
        }
        headers.push(line);
    }
    let header = |name: &str| {
        headers.iter().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    if header("expect").is_some_and(|value| value.eq_ignore_ascii_case("100-continue")) {
        let mut writer = stream.try_clone().ok()?;
        writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").ok()?;
    }
    let length: usize = header("content-length")?.parse().ok()?;
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).ok()?;
    Some((headers, String::from_utf8(body).ok()?))
}

fn count_breakpoints(value: &Value) -> usize {
    match value {
        Value::Object(entries) => entries
            .iter()
            .map(|(key, value)| usize::from(key == "cache_control") + count_breakpoints(value))
            .sum(),
        Value::Array(items) => items.iter().map(count_breakpoints).sum(),
        _ => 0,
    }
}

fn check(body: &Value) -> Result<usize, String> {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("no messages")?;
    if body.get("system").is_none()
        || body.get("tools").is_none()
        || body.get("max_tokens").and_then(Value::as_u64).is_none()
    {
        return Err("a request needs system, tools and max_tokens".into());
    }
    let breakpoints = count_breakpoints(body);
    if breakpoints > CACHE_BREAKPOINTS_MAX {
        return Err(format!(
            "{breakpoints} cache breakpoints, at most {CACHE_BREAKPOINTS_MAX}"
        ));
    }
    let mut assistants = 0;
    let mut open: Vec<String> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or("a message has no role")?;
        let expected = if index % 2 == 0 { "user" } else { "assistant" };
        if role != expected {
            return Err(format!("message {index} is {role}, expected {expected}"));
        }
        let content = message
            .get("content")
            .and_then(Value::as_array)
            .ok_or("a message has no content")?;
        if content.is_empty() {
            return Err(format!("message {index} is empty"));
        }
        let results: Vec<String> = content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            .filter_map(|block| {
                block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        if role == "user" && results != open {
            return Err(format!(
                "message {index} answers {results:?}, the tool uses before it were {open:?}"
            ));
        }
        open = content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            .filter_map(|block| block.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect();
        assistants += usize::from(role == "assistant");
    }
    if !open.is_empty() {
        return Err("the last message leaves tool uses unanswered".into());
    }
    Ok(assistants)
}

fn answer(headers: &[String], body: &str, steps: &[Value]) -> Result<Value, String> {
    if !headers
        .iter()
        .any(|line| line.to_ascii_lowercase().starts_with("x-api-key:"))
    {
        return Err("no x-api-key header".into());
    }
    let body = Value::decode(body).map_err(|error| format!("body: {error}"))?;
    let step = check(&body)?;
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .map_or(0, <[Value]>::len) as u64;
    let usage = Value::object([
        (
            "input_tokens",
            Value::number(USAGE_INPUT_PER_MESSAGE * messages),
        ),
        ("output_tokens", Value::number(USAGE_OUTPUT)),
        ("cache_read_input_tokens", Value::number(USAGE_CACHE_READ)),
        (
            "cache_creation_input_tokens",
            Value::number(USAGE_INPUT_PER_MESSAGE),
        ),
    ]);
    let child = is_child(&body);
    let script = steps
        .get(step)
        .ok_or_else(|| format!("the script has no step {step}"))?;
    let all = body
        .get("messages")
        .and_then(Value::as_array)
        .unwrap_or(&[]);
    let mut content = vec![Value::object([
        ("type", Value::text("text")),
        (
            "text",
            script
                .get("text")
                .or(script.get("answer"))
                .cloned()
                .unwrap_or(Value::text("")),
        ),
    ])];
    let calls = script.get("calls").and_then(Value::as_array).unwrap_or(&[]);
    for (index, call) in calls.iter().enumerate() {
        content.push(Value::object([
            ("type", Value::text("tool_use")),
            (
                "id",
                Value::String(format!(
                    "toolu_{}{step}_{index}",
                    if child { "child_" } else { "" }
                )),
            ),
            (
                "name",
                call.get("name")
                    .cloned()
                    .ok_or("a scripted call has no name")?,
            ),
            (
                "input",
                filled(
                    call.get("input").ok_or("a scripted call has no input")?,
                    all,
                ),
            ),
        ]));
    }
    let stop = match script.get("stop").and_then(Value::as_str) {
        Some(stop) => stop,
        None if calls.is_empty() => "end_turn",
        None => "tool_use",
    };
    Ok(Value::object([
        ("id", Value::String(format!("msg_{step}"))),
        ("type", Value::text("message")),
        ("role", Value::text("assistant")),
        ("content", Value::Array(content)),
        ("stop_reason", Value::text(stop)),
        ("usage", usage),
    ]))
}
