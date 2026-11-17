#[path = "../json.rs"]
#[allow(dead_code)]
mod json;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use json::Value;

const HEADER_LINES_MAX: usize = 128;
const BYTES_PER_TOKEN: usize = 4;
const CACHE_MIN_TOKENS: u64 = 1024;
const CACHE_INCREMENT_TOKENS: u64 = 128;
const OUTPUT_TOKENS: u64 = 40;
const REASONING_TOKENS: u64 = 16;
const ENCRYPTED_PREFIX: &str = "enc-mock-";
const RESPONSES_PATH: &str = "/v1/responses";
const REQUIRED_INCLUDE: &str = "reasoning.encrypted_content";

#[derive(Default)]
struct Server {
    issued: BTreeSet<String>,
    faults: BTreeSet<(usize, String)>,
    prefixes: BTreeMap<String, String>,
    requests: u64,
}

fn steps_of(path: &str) -> Vec<Value> {
    let script = Value::decode(&std::fs::read_to_string(path).expect("read the script"))
        .expect("the script is JSON");
    script
        .as_array()
        .expect("the script is a list of steps")
        .to_vec()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [port_file, script, delay_ms] = args.as_slice() else {
        eprintln!("usage: mock-responses PORT_FILE SCRIPT_JSON DELAY_MS");
        std::process::exit(2);
    };
    let steps = Arc::new(steps_of(script));
    let delay = Duration::from_millis(delay_ms.parse().expect("DELAY_MS is a number"));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
    let port = listener.local_addr().expect("a bound address").port();
    std::fs::write(format!("{port_file}.new"), port.to_string()).expect("write the port");
    std::fs::rename(format!("{port_file}.new"), port_file).expect("publish the port");
    let server = Arc::new(Mutex::new(Server::default()));
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let steps = Arc::clone(&steps);
        let server = Arc::clone(&server);
        std::thread::spawn(move || serve(stream, &steps, delay, &server));
    }
}

fn read_request(stream: &TcpStream) -> Option<(String, Vec<String>, String)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
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
    let length: usize = headers.iter().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())
            .flatten()
    })?;
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).ok()?;
    let path = request_line.split_whitespace().nth(1)?.to_owned();
    Some((path, headers, String::from_utf8(body).ok()?))
}

fn respond(stream: &mut TcpStream, status: u16, reason: &str, extra: &str, body: &Value) {
    let text = body.encode();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn error_body(message: &str, kind: &str, code: &str) -> Value {
    Value::object([(
        "error",
        Value::object([
            ("message", Value::text(message)),
            ("type", Value::text(kind)),
            ("param", Value::Null),
            ("code", Value::text(code)),
        ]),
    )])
}

fn kind(item: &Value) -> &str {
    item.get("type")
        .and_then(Value::as_str)
        .unwrap_or("message")
}

fn check(headers: &[String], body: &Value, server: &Server) -> Result<usize, String> {
    if !headers.iter().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("authorization: bearer ")
    }) {
        return Err("You didn't provide an API key.".into());
    }
    for key in [
        "model",
        "instructions",
        "input",
        "tools",
        "prompt_cache_key",
    ] {
        if body.get(key).is_none() {
            return Err(format!("Missing required parameter: '{key}'."));
        }
    }
    if !matches!(body.get("store"), Some(Value::Bool(false)))
        || !matches!(body.get("stream"), Some(Value::Bool(true)))
    {
        return Err("this fake serves only store=false streaming requests".into());
    }
    let included = body
        .get("include")
        .and_then(Value::as_array)
        .is_some_and(|include| {
            include
                .iter()
                .any(|value| value.as_str() == Some(REQUIRED_INCLUDE))
        });
    if !included {
        return Err("a stateless request must include reasoning.encrypted_content".into());
    }
    let tools: BTreeMap<String, String> = body
        .get("tools")
        .and_then(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(|tool| {
            Some((
                tool.get("name")?.as_str()?.to_owned(),
                tool.get("type")?.as_str()?.to_owned(),
            ))
        })
        .collect();
    let input = body
        .get("input")
        .and_then(Value::as_array)
        .ok_or("Invalid 'input': expected an array.")?;
    let mut open: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut answered: BTreeSet<String> = BTreeSet::new();
    let mut reasonings = 0;
    let mut last_reasoning: Option<String> = None;
    for (index, item) in input.iter().enumerate() {
        if let Some(id) = item.get("id").and_then(Value::as_str)
            && !id.starts_with("rs_")
        {
            return Err(format!(
                "Item with id '{id}' not found. Items are not persisted when 'store' is set to false. Try again with 'store' set to true, or remove this item from your input."
            ));
        }
        match kind(item) {
            "reasoning" => {
                let encrypted = item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !server.issued.contains(encrypted) {
                    return Err(format!(
                        "The encrypted content for item input[{index}] could not be verified."
                    ));
                }
                reasonings += 1;
                last_reasoning = Some(encrypted.to_owned());
            }
            "function_call" | "custom_tool_call" => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                let expected = if kind(item) == "function_call" {
                    "function"
                } else {
                    "custom"
                };
                if tools.get(name).map(String::as_str) != Some(expected) {
                    return Err(format!(
                        "input[{index}] calls {name}, which the request's tools do not declare as a {expected} tool"
                    ));
                }
                let issued_with = call_id
                    .split_once("-call-")
                    .map(|(step, _)| step.to_owned())
                    .unwrap_or_default();
                if last_reasoning.as_deref().map(|encrypted| {
                    encrypted
                        .trim_start_matches(ENCRYPTED_PREFIX)
                        .split('-')
                        .next()
                        .unwrap_or("")
                }) != Some(issued_with.trim_start_matches("call_"))
                {
                    return Err(format!(
                        "Item 'input[{index}]' of type '{}' was provided without its required 'reasoning' item.",
                        kind(item)
                    ));
                }
                if open
                    .insert(
                        call_id.clone(),
                        if expected == "function" {
                            "function_call_output"
                        } else {
                            "custom_tool_call_output"
                        },
                    )
                    .is_some()
                {
                    return Err(format!("Duplicate call_id {call_id}."));
                }
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
                match open.get(call_id) {
                    None => {
                        return Err(format!(
                            "No tool call found for function call output with call_id {call_id}."
                        ));
                    }
                    Some(output) if *output != kind(item) => {
                        return Err(format!(
                            "input[{index}] answers {call_id} with {}, expected {output}",
                            kind(item)
                        ));
                    }
                    Some(_) => {}
                }
                if !answered.insert(call_id.to_owned()) {
                    return Err(format!("Duplicate output for call_id {call_id}."));
                }
            }
            "message" => {}
            other => return Err(format!("Invalid input item type {other}")),
        }
    }
    if let Some(unanswered) = open.keys().find(|call| !answered.contains(*call)) {
        return Err(format!(
            "No tool output found for function call {unanswered}."
        ));
    }
    Ok(reasonings)
}

fn sse(events: &mut Vec<String>, kind: &str, data: Value) {
    let mut data = data;
    data.insert("type", Value::text(kind));
    data.insert("sequence_number", Value::number(events.len() as u64));
    events.push(format!("event: {kind}\ndata: {}\n\n", data.encode()));
}

fn halves(text: &str) -> (String, String) {
    let mut middle = text.len() / 2;
    while !text.is_char_boundary(middle) {
        middle += 1;
    }
    (text[..middle].to_owned(), text[middle..].to_owned())
}

fn cached_tokens(server: &mut Server, key: &str, prefix: String) -> u64 {
    let previous = server
        .prefixes
        .insert(key.to_owned(), prefix.clone())
        .unwrap_or_default();
    let common = previous
        .bytes()
        .zip(prefix.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let tokens = (common / BYTES_PER_TOKEN) as u64;
    if tokens < CACHE_MIN_TOKENS {
        return 0;
    }
    tokens - tokens % CACHE_INCREMENT_TOKENS
}

fn response_events(
    step: usize,
    script: &Value,
    body: &Value,
    server: &mut Server,
) -> Result<Vec<String>, String> {
    if let Some(expected) = script.get("expect").and_then(Value::as_str) {
        let said = body
            .get("input")
            .and_then(Value::as_array)
            .unwrap_or(&[])
            .iter()
            .filter(|item| item.get("role").and_then(Value::as_str) == Some("user"))
            .any(|item| item.encode().contains(expected));
        if !said {
            return Err(format!(
                "step {step} expects a user message holding {expected:?}"
            ));
        }
    }
    let nonce = server.requests;
    let id = format!("resp_{step}_{nonce}");
    let mut items = Vec::new();
    let encrypted = format!("{ENCRYPTED_PREFIX}{step}-{nonce}");
    server.issued.insert(encrypted.clone());
    items.push(Value::object([
        ("id", Value::String(format!("rs_{step}_{nonce}"))),
        ("type", Value::text("reasoning")),
        ("summary", Value::Array(Vec::new())),
        ("encrypted_content", Value::String(encrypted)),
    ]));
    let words = script
        .get("text")
        .or(script.get("answer"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !words.is_empty() {
        items.push(Value::object([
            ("id", Value::String(format!("msg_{step}_{nonce}"))),
            ("type", Value::text("message")),
            ("status", Value::text("completed")),
            ("role", Value::text("assistant")),
            (
                "content",
                Value::Array(vec![Value::object([
                    ("type", Value::text("output_text")),
                    ("text", Value::text(words)),
                    ("annotations", Value::Array(Vec::new())),
                ])]),
            ),
        ]));
    }
    for (index, call) in script
        .get("calls")
        .and_then(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let name = call
            .get("name")
            .and_then(Value::as_str)
            .ok_or("a scripted call has no name")?;
        let call_id = format!("call_{step}-call-{index}");
        let item = match call.get("patch").and_then(Value::as_str) {
            Some(patch) => Value::object([
                ("id", Value::String(format!("ctc_{step}_{index}"))),
                ("type", Value::text("custom_tool_call")),
                ("status", Value::text("completed")),
                ("call_id", Value::String(call_id)),
                ("name", Value::text(name)),
                ("input", Value::text(patch)),
            ]),
            None => Value::object([
                ("id", Value::String(format!("fc_{step}_{index}"))),
                ("type", Value::text("function_call")),
                ("status", Value::text("completed")),
                ("call_id", Value::String(call_id)),
                ("name", Value::text(name)),
                (
                    "arguments",
                    Value::String(
                        call.get("input")
                            .cloned()
                            .unwrap_or(Value::object([]))
                            .encode(),
                    ),
                ),
            ]),
        };
        items.push(item);
    }
    let input = body.get("input").map(Value::encode).unwrap_or_default();
    let prefix = format!(
        "{}{}{}",
        body.get("instructions")
            .map(Value::encode)
            .unwrap_or_default(),
        body.get("tools").map(Value::encode).unwrap_or_default(),
        input
    );
    let input_tokens = (prefix.len() / BYTES_PER_TOKEN) as u64;
    let key = body
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let cached = cached_tokens(server, &key, prefix);
    let mut events = Vec::new();
    let head = Value::object([
        ("id", Value::text(&id)),
        ("object", Value::text("response")),
        ("status", Value::text("in_progress")),
        ("output", Value::Array(Vec::new())),
    ]);
    sse(
        &mut events,
        "response.created",
        Value::object([("response", head.clone())]),
    );
    sse(
        &mut events,
        "response.in_progress",
        Value::object([("response", head)]),
    );
    for (index, item) in items.iter().enumerate() {
        let at = Value::number(index as u64);
        let item_id = item.get("id").cloned().unwrap_or(Value::Null);
        let mut added = item.clone();
        match kind(item) {
            "message" => {
                added.insert("content", Value::Array(Vec::new()));
                sse(
                    &mut events,
                    "response.output_item.added",
                    Value::object([("output_index", at.clone()), ("item", added)]),
                );
                let (first, second) = halves(words);
                for part in [first, second] {
                    sse(
                        &mut events,
                        "response.output_text.delta",
                        Value::object([
                            ("item_id", item_id.clone()),
                            ("output_index", at.clone()),
                            ("content_index", Value::number(0)),
                            ("delta", Value::String(part)),
                        ]),
                    );
                }
            }
            "function_call" => {
                added.insert("arguments", Value::text(""));
                sse(
                    &mut events,
                    "response.output_item.added",
                    Value::object([("output_index", at.clone()), ("item", added)]),
                );
                let (first, second) =
                    halves(item.get("arguments").and_then(Value::as_str).unwrap_or(""));
                for part in [first, second] {
                    sse(
                        &mut events,
                        "response.function_call_arguments.delta",
                        Value::object([
                            ("item_id", item_id.clone()),
                            ("output_index", at.clone()),
                            ("delta", Value::String(part)),
                        ]),
                    );
                }
            }
            "custom_tool_call" => {
                added.insert("input", Value::text(""));
                sse(
                    &mut events,
                    "response.output_item.added",
                    Value::object([("output_index", at.clone()), ("item", added)]),
                );
                let (first, second) =
                    halves(item.get("input").and_then(Value::as_str).unwrap_or(""));
                for part in [first, second] {
                    sse(
                        &mut events,
                        "response.custom_tool_call_input.delta",
                        Value::object([
                            ("item_id", item_id.clone()),
                            ("output_index", at.clone()),
                            ("delta", Value::String(part)),
                        ]),
                    );
                }
            }
            _ => sse(
                &mut events,
                "response.output_item.added",
                Value::object([("output_index", at.clone()), ("item", added)]),
            ),
        }
        sse(
            &mut events,
            "response.output_item.done",
            Value::object([("output_index", at), ("item", item.clone())]),
        );
    }
    let usage = Value::object([
        ("input_tokens", Value::number(input_tokens)),
        (
            "input_tokens_details",
            Value::object([("cached_tokens", Value::number(cached))]),
        ),
        ("output_tokens", Value::number(OUTPUT_TOKENS)),
        (
            "output_tokens_details",
            Value::object([("reasoning_tokens", Value::number(REASONING_TOKENS))]),
        ),
        ("total_tokens", Value::number(input_tokens + OUTPUT_TOKENS)),
    ]);
    let incomplete = script.get("incomplete").and_then(Value::as_str);
    let mut done = Value::object([
        ("id", Value::text(&id)),
        ("object", Value::text("response")),
        (
            "status",
            Value::text(if incomplete.is_some() {
                "incomplete"
            } else {
                "completed"
            }),
        ),
        ("output", Value::Array(items)),
        ("usage", usage),
    ]);
    if let Some(reason) = incomplete {
        done.insert(
            "incomplete_details",
            Value::object([("reason", Value::text(reason))]),
        );
    }
    let terminal = if incomplete.is_some() {
        "response.incomplete"
    } else {
        "response.completed"
    };
    sse(&mut events, terminal, Value::object([("response", done)]));
    Ok(events)
}

fn fault(server: &mut Server, step: usize, script: &Value) -> Option<String> {
    let faults = script.get("faults").and_then(Value::as_array)?;
    faults
        .iter()
        .filter_map(Value::as_str)
        .find(|fault| server.faults.insert((step, (*fault).to_owned())))
        .map(str::to_owned)
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

fn serve(mut stream: TcpStream, steps: &[Value], delay: Duration, server: &Mutex<Server>) {
    let Some((path, headers, body)) = read_request(&stream) else {
        return;
    };
    log_request(&body);
    if path != RESPONSES_PATH {
        respond(
            &mut stream,
            404,
            "Not Found",
            "",
            &error_body(
                &format!("Invalid URL (POST {path})"),
                "invalid_request_error",
                "unknown_url",
            ),
        );
        return;
    }
    let Ok(body) = Value::decode(&body) else {
        respond(
            &mut stream,
            400,
            "Bad Request",
            "",
            &error_body(
                "We could not parse the JSON body of your request.",
                "invalid_request_error",
                "invalid_json",
            ),
        );
        return;
    };
    let planned = {
        let mut server = server.lock().expect("the server state is never poisoned");
        server.requests += 1;
        check(&headers, &body, &server).and_then(|step| {
            let script = steps
                .get(step)
                .ok_or_else(|| format!("the script has no step {step}"))?;
            let fault = fault(&mut server, step, script);
            let events = response_events(step, script, &body, &mut server)?;
            Ok((step, fault, events))
        })
    };
    let (step, fault, events) = match planned {
        Ok(planned) => planned,
        Err(error) => {
            eprintln!("mock-responses: 400 {error}");
            respond(
                &mut stream,
                400,
                "Bad Request",
                "",
                &error_body(&error, "invalid_request_error", "invalid_request"),
            );
            return;
        }
    };
    eprintln!("mock-responses: step {step} fault {fault:?}");
    match fault.as_deref() {
        Some("throttle") => {
            respond(
                &mut stream,
                429,
                "Too Many Requests",
                "",
                &error_body(
                    "Rate limit reached for requests. Please try again in 1.2s.",
                    "requests",
                    "rate_limit_exceeded",
                ),
            );
            return;
        }
        Some("overloaded") => {
            respond(
                &mut stream,
                503,
                "Service Unavailable",
                "retry-after-ms: 300\r\n",
                &error_body(
                    "The server is overloaded.",
                    "server_error",
                    "server_is_overloaded",
                ),
            );
            return;
        }
        _ => {}
    }
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let delay = steps
        .get(step)
        .and_then(|script| script.get("delay_ms"))
        .and_then(Value::as_u64)
        .map_or(delay, Duration::from_millis);
    let pause = delay / u32::try_from(events.len()).expect("a scripted response has few events");
    let cut = events.len() / 2;
    for (index, event) in events.iter().enumerate() {
        std::thread::sleep(pause);
        if fault.as_deref() == Some("cut") && index == cut {
            return;
        }
        if fault.as_deref() == Some("failed") && index == cut {
            let mut tail = Vec::new();
            sse(
                &mut tail,
                "response.failed",
                Value::object([(
                    "response",
                    Value::object([
                        ("status", Value::text("failed")),
                        (
                            "error",
                            Value::object([
                                ("code", Value::text("server_error")),
                                ("message", Value::text("The model produced an error.")),
                            ]),
                        ),
                    ]),
                )]),
            );
            let _ = stream.write_all(tail[0].as_bytes());
            return;
        }
        if stream.write_all(event.as_bytes()).is_err() {
            return;
        }
    }
}
