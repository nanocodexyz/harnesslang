use crate::run::json::Json;
use crate::wire::json::Value;
use crate::wire::{self, Attached, Usage};

pub const HTTP_OK: u64 = 200;
pub const HTTP_BAD_REQUEST: u64 = 400;
pub const HTTP_REQUEST_TIMEOUT: u64 = 408;
pub const HTTP_CONFLICT: u64 = 409;
pub const HTTP_TOO_LARGE: u64 = 413;
pub const HTTP_TOO_MANY: u64 = 429;
pub const HTTP_SERVER_ERROR: u64 = 500;
pub const IMAGE_ATTACHED: &str = "The image is attached in the next message.";
const RETRY_HINT: &str = "try again in";
const QUOTA_CODES: [&str; 5] = [
    "insufficient_quota",
    "credit_balance_exhausted",
    "organization_spend_limit_exceeded",
    "project_spend_limit_exceeded",
    "usage_not_included",
];
const THROTTLE_CODES: [&str; 4] = [
    "rate_limit_exceeded",
    "slow_down",
    "server_is_overloaded",
    "policy_capacity_unavailable",
];
const REJECTED_CODES: [&str; 5] = [
    "invalid_prompt",
    "content_policy_violation",
    "cyber_policy",
    "bio_policy",
    "budget_reached",
];
const CONTEXT_EXCEEDED: &str = "context_length_exceeded";
const INCOMPLETE: [(&str, &str); 3] = [
    ("max_tokens", "max_output_tokens"),
    ("refusal", "content_filter"),
    ("pause", "pause"),
];

fn message(role: &str, kind: &str, part: (&str, Value)) -> Value {
    let mut content = Value::object([("type", Value::text(kind))]);
    content.insert(part.0, part.1);
    Value::object([
        ("type", Value::text("message")),
        ("role", Value::text(role)),
        ("content", Value::Array(vec![content])),
    ])
}

fn image_message(media_type: &str, data: &str) -> Value {
    let url = format!("data:{media_type};base64,{data}");
    message("user", "input_image", ("image_url", Value::String(url)))
}

fn user_message(text: &str) -> Value {
    message("user", "input_text", ("text", Value::text(text)))
}

fn assistant_message(text: &str) -> Value {
    message("assistant", "output_text", ("text", Value::text(text)))
}

fn copied(item: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|key| {
                item.get(key)
                    .map(|value| ((*key).to_owned(), value.clone()))
            })
            .collect(),
    )
}

fn input_item(item: &Value) -> Option<Value> {
    match item.get("type").and_then(Value::as_str)? {
        "reasoning" => {
            item.get("encrypted_content").and_then(Value::as_str)?;
            let mut kept = copied(item, &["type", "id", "summary", "encrypted_content"]);
            if kept.get("summary").is_none() {
                kept.insert("summary", Value::Array(Vec::new()));
            }
            Some(kept)
        }
        "message" => {
            let parts: Vec<Value> = item
                .get("content")
                .and_then(Value::as_array)
                .unwrap_or(&[])
                .iter()
                .filter(|part| {
                    matches!(
                        part.get("type").and_then(Value::as_str),
                        Some("output_text" | "refusal")
                    )
                })
                .map(|part| copied(part, &["type", "text", "refusal"]))
                .collect();
            if parts.is_empty() {
                return None;
            }
            Some(Value::object([
                ("type", Value::text("message")),
                ("role", Value::text("assistant")),
                ("content", Value::Array(parts)),
            ]))
        }
        "function_call" => Some(copied(item, &["type", "call_id", "name", "arguments"])),
        "custom_tool_call" => Some(copied(item, &["type", "call_id", "name", "input"])),
        _ => None,
    }
}

struct Input {
    items: Vec<Value>,
    custom: Vec<String>,
}

impl Input {
    fn output(&mut self, call: &Value, text: &str) -> Result<(), String> {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .ok_or("a call has no id")?;
        let kind = if self.custom.iter().any(|custom| custom == id) {
            "custom_tool_call_output"
        } else {
            "function_call_output"
        };
        self.items.push(Value::object([
            ("type", Value::text(kind)),
            ("call_id", Value::text(id)),
            ("output", Value::text(text)),
        ]));
        Ok(())
    }

    fn reply(&mut self, reply: &Value) -> Result<(), String> {
        let (name, payload) = wire::tagged(reply).ok_or("an assistant entry has no reply")?;
        let fields = payload
            .and_then(Value::as_array)
            .ok_or("a reply has no fields")?;
        let (words, raw) = match name {
            "message" => (fields.first(), fields.get(1)),
            "calls" => (fields.get(1), fields.get(2)),
            _ => return Err("context holds a reply this wire does not know".to_owned()),
        };
        let raw = raw.and_then(Value::as_str).unwrap_or("");
        if raw.is_empty() {
            let words = words.and_then(Value::as_str).unwrap_or("");
            if !words.is_empty() {
                self.items.push(assistant_message(words));
            }
            if name == "calls" {
                for call in fields[0].as_array().unwrap_or(&[]) {
                    self.items.push(function_call(call));
                }
            }
            return Ok(());
        }
        let decoded = Value::decode(raw).ok_or("a reply's items are not JSON")?;
        for item in decoded.as_array().ok_or("a reply's items are not a list")? {
            let Some(kept) = input_item(item) else {
                continue;
            };
            if kept.get("type").and_then(Value::as_str) == Some("custom_tool_call")
                && let Some(id) = kept.get("call_id").and_then(Value::as_str)
            {
                self.custom.push(id.to_owned());
            }
            self.items.push(kept);
        }
        Ok(())
    }
}

fn function_call(call: &Value) -> Value {
    let field = |key: &str| Value::text(call.get(key).and_then(Value::as_str).unwrap_or(""));
    Value::object([
        ("type", Value::text("function_call")),
        ("call_id", field("id")),
        ("name", field("tool")),
        ("arguments", field("input")),
    ])
}

pub fn render(context: &Value, attached: &Attached<'_>) -> Result<Vec<Value>, String> {
    let mut input = Input {
        items: Vec::new(),
        custom: Vec::new(),
    };
    for entry in context.as_array().ok_or("the context is not a list")? {
        let (name, payload) = wire::tagged(entry).ok_or("a context entry is not a variant")?;
        let pair = || -> Result<(&Value, &Value), String> {
            match payload.and_then(Value::as_array) {
                Some([call, second]) => Ok((call, second)),
                _ => Err(format!("context entry {name} lacks its 2 fields")),
            }
        };
        let text = || payload.and_then(Value::as_str);
        match name {
            "user" => input
                .items
                .push(user_message(text().ok_or("a user entry has no text")?)),
            "notice" => input
                .items
                .push(user_message(text().ok_or("a notice entry has no text")?)),
            "assistant" => input.reply(payload.ok_or("an assistant entry has no reply")?)?,
            "result" | "error" => {
                let (call, output) = pair()?;
                input.output(call, output.as_str().unwrap_or(""))?;
            }
            "attached" => {
                let (call, blob) = pair()?;
                let digest = blob
                    .get("blob")
                    .and_then(Value::as_str)
                    .ok_or("an attached entry has no blob digest")?;
                let (media_type, data) = attached(digest)
                    .ok_or("context holds a blob which the session does not hold")?;
                input.output(call, IMAGE_ATTACHED)?;
                input.items.push(image_message(&media_type, &data));
            }
            "unknown" => input.output(
                payload.ok_or("an unknown entry has no call")?,
                wire::UNKNOWN_OUTCOME,
            )?,
            _ => return Err("context holds an entry this wire does not know".to_owned()),
        }
    }
    Ok(input.items)
}

fn hinted_ms(message: &str) -> u64 {
    let lower = message.to_ascii_lowercase();
    let Some(at) = lower.find(RETRY_HINT) else {
        return 0;
    };
    let rest = lower[at + RETRY_HINT.len()..].trim_start();
    let number = &rest[..rest
        .bytes()
        .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
        .count()];
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if fraction.contains('.') || (whole.is_empty() && fraction.is_empty()) {
        return 0;
    }
    let unit = rest[number.len()..].trim_start();
    let scale = if unit.starts_with("ms") {
        1
    } else if unit.starts_with('s') {
        wire::MILLISECONDS_PER_SECOND
    } else {
        return 0;
    };
    let digits = |text: &str| {
        text.bytes().fold(0_u64, |total, digit| {
            total
                .saturating_mul(10)
                .saturating_add(u64::from(digit - b'0'))
        })
    };
    let kept = if scale == 1 { 0 } else { fraction.len().min(3) };
    let mut ms = digits(whole).saturating_mul(scale);
    let mut place = scale;
    for digit in fraction[..kept].bytes() {
        place /= 10;
        ms = ms.saturating_add(u64::from(digit - b'0') * place);
    }
    if fraction[kept..].bytes().any(|digit| digit != b'0') {
        ms = ms.saturating_add(1);
    }
    ms
}

fn empty_message() -> Json {
    wire::tagged_json(
        "message",
        vec![Json::text(""), Json::text(""), Json::Bool(true)],
    )
}

fn classify(status: Option<u64>, code: &str, message: &str, retry_after_ms: u64) -> Json {
    let text = match status {
        Some(status) => format!("HTTP {status} {code}: {message}"),
        None => format!("response failed {code}: {message}"),
    };
    if code == CONTEXT_EXCEEDED || status == Some(HTTP_TOO_LARGE) {
        return wire::ok(empty_message(), "context_exceeded", Usage::default());
    }
    if QUOTA_CODES.contains(&code) || REJECTED_CODES.contains(&code) {
        return wire::rejected(&text);
    }
    let after = if retry_after_ms > 0 {
        retry_after_ms
    } else {
        hinted_ms(message)
    };
    if THROTTLE_CODES.contains(&code) || status == Some(HTTP_TOO_MANY) {
        return wire::throttled(&text, after);
    }
    match status {
        Some(status)
            if matches!(status, HTTP_REQUEST_TIMEOUT | HTTP_CONFLICT)
                || status >= HTTP_SERVER_ERROR =>
        {
            wire::transient(&text, after)
        }
        Some(_) => wire::rejected(&text),
        None => wire::transient(&text, after),
    }
}

fn error_fields(error: Option<&Value>) -> (String, String) {
    let field = |key: &str| {
        error
            .and_then(|error| error.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let code = field("code");
    let code = if code.is_empty() { field("type") } else { code };
    (code, field("message"))
}

fn usage_of(response: &Value) -> Usage {
    let usage = response.get("usage");
    let field = |path: &[&str]| {
        let mut at = usage;
        for key in path {
            at = at.and_then(|value| value.get(key));
        }
        at.and_then(Value::as_u64).unwrap_or(0)
    };
    let input = field(&["input_tokens"]);
    let cached = field(&["input_tokens_details", "cached_tokens"]);
    Usage {
        input: input.saturating_sub(cached),
        output: field(&["output_tokens"]),
        cache_read: cached,
        cache_write: 0,
    }
}

fn call_input(arguments: &str) -> String {
    match Value::decode(arguments) {
        Some(Value::Object(entries)) => Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| match value {
                    Value::Array(_) | Value::Object(_) => (key, Value::String(value.encode())),
                    other => (key, other),
                })
                .collect(),
        )
        .encode(),
        _ => arguments.to_owned(),
    }
}

fn reply_of(items: &[Value], end_turn: bool, incomplete: Option<&str>) -> (Json, &'static str) {
    let mut words = String::new();
    let mut refused = false;
    let mut calls = Vec::new();
    for item in items {
        let field = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or("");
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item.get("content").and_then(Value::as_array).unwrap_or(&[]) {
                    let text = |key: &str| part.get(key).and_then(Value::as_str).unwrap_or("");
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => words.push_str(text("text")),
                        Some("refusal") => {
                            refused = true;
                            words.push_str(text("refusal"));
                        }
                        _ => {}
                    }
                }
            }
            Some("function_call") => {
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                calls.push(wire::call(
                    field("call_id"),
                    field("name"),
                    &call_input(arguments),
                ));
            }
            Some("custom_tool_call") => {
                let input = Value::object([("input", Value::text(field("input")))]).encode();
                calls.push(wire::call(field("call_id"), field("name"), &input));
            }
            _ => {}
        }
    }
    let kept: Vec<Value> = items.iter().filter_map(input_item).collect();
    let raw = Json::String(Value::Array(kept).encode());
    let stop = match incomplete {
        Some("max_output_tokens") => "max_tokens",
        Some("content_filter") => "refusal",
        Some(_) => "pause",
        None if refused => "refusal",
        None if calls.is_empty() => "end",
        None => "tool_use",
    };
    let reply = if calls.is_empty() {
        wire::tagged_json(
            "message",
            vec![Json::String(words), raw, Json::Bool(end_turn)],
        )
    } else {
        wire::tagged_json("calls", vec![Json::Array(calls), Json::String(words), raw])
    };
    (reply, stop)
}

struct Assembly {
    items: Vec<(u64, Value)>,
    terminal: Option<Value>,
    failed: Option<Value>,
}

fn assemble(events: &[String]) -> Result<Assembly, String> {
    let mut assembly = Assembly {
        items: Vec::new(),
        terminal: None,
        failed: None,
    };
    for data in events
        .iter()
        .map(|data| data.trim())
        .filter(|data| !data.is_empty() && *data != "[DONE]")
    {
        let event = Value::decode(data).ok_or("stream event is not JSON")?;
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_item.done") => {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(assembly.items.len() as u64);
                let item = event
                    .get("item")
                    .cloned()
                    .ok_or("stream output_item.done event has no item")?;
                assembly.items.push((index, item));
            }
            Some("response.completed" | "response.incomplete") => {
                assembly.terminal = Some(event);
            }
            Some("response.failed") => {
                assembly.failed = Some(
                    event
                        .get("response")
                        .and_then(|response| response.get("error"))
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }
            Some("error") => {
                assembly.failed =
                    Some(event.get("error").cloned().unwrap_or_else(|| event.clone()));
            }
            _ => {}
        }
    }
    assembly.items.sort_by_key(|(index, _)| *index);
    Ok(assembly)
}

fn interpret(events: &[String]) -> Json {
    let assembly = match assemble(events) {
        Ok(assembly) => assembly,
        Err(error) => return wire::transient(&error, 0),
    };
    if let Some(error) = assembly.failed {
        let (code, message) = error_fields(Some(&error));
        return classify(None, &code, &message, 0);
    }
    let Some(terminal) = assembly.terminal else {
        return wire::transient("stream closed before response.completed", 0);
    };
    let response = terminal.get("response").cloned().unwrap_or(Value::Null);
    let used = usage_of(&response);
    let incomplete = (terminal.get("type").and_then(Value::as_str) == Some("response.incomplete"))
        .then(|| {
            response
                .get("incomplete_details")
                .and_then(|details| details.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        });
    let items: Vec<Value> = if assembly.items.is_empty() {
        response
            .get("output")
            .and_then(Value::as_array)
            .unwrap_or(&[])
            .to_vec()
    } else {
        assembly.items.into_iter().map(|(_, item)| item).collect()
    };
    let end_turn = !matches!(response.get("end_turn"), Some(Value::Bool(false)));
    let (reply, stop) = reply_of(&items, end_turn, incomplete);
    wire::ok(reply, stop, used)
}

pub fn decode(http: &wire::Http) -> Json {
    match (&http.payload, http.status) {
        (wire::Payload::Events(events), HTTP_OK) => interpret(events),
        (wire::Payload::Body(body), HTTP_OK) => interpret(
            &body
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        ),
        (payload, status) => {
            let body = match payload {
                wire::Payload::Body(body) => body.as_str(),
                wire::Payload::Events(_) => "",
            };
            let decoded = Value::decode(body);
            let (code, message) =
                error_fields(decoded.as_ref().and_then(|value| value.get("error")));
            let message = if message.is_empty() {
                body.to_owned()
            } else {
                message
            };
            classify(
                Some(status),
                &code,
                &wire::with_challenge(status, &http.headers, &message),
                wire::retry_after_ms(&http.headers),
            )
        }
    }
}

fn items(reply: &Json) -> (Vec<Value>, bool) {
    let Some((name, Json::Array(fields))) = reply.tagged() else {
        return (Vec::new(), true);
    };
    let text = |at: usize| fields.get(at).and_then(Json::as_str).unwrap_or("");
    let end_turn = name != "message" || fields.get(2) != Some(&Json::Bool(false));
    let raw = if name == "calls" { text(2) } else { text(1) };
    if let Some(Value::Array(items)) = Value::decode(raw) {
        return (items, end_turn);
    }
    let words = if name == "calls" { text(1) } else { text(0) };
    let mut out = Vec::new();
    if !words.is_empty() {
        out.push(assistant_message(words));
    }
    if name == "calls"
        && let Some(Json::Array(calls)) = fields.first()
    {
        for call in calls {
            out.push(function_call(&Value::of_runtime(call)));
        }
    }
    (out, end_turn)
}

fn event(kind: &str, mut fields: Vec<(&str, Value)>) -> String {
    fields.insert(0, ("type", Value::text(kind)));
    Value::object(fields).encode()
}

pub fn encode(attempt: &Json) -> wire::Answer {
    let Some((name, payload)) = attempt.tagged() else {
        return wire::Answer::Transport("the simulator drew no attempt".to_owned());
    };
    let fields: &[Json] = match payload {
        Json::Array(items) => items,
        _ => &[],
    };
    let text = |at: usize| {
        fields
            .get(at)
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let number = |at: usize| fields.get(at).and_then(Json::as_u64).unwrap_or(0);
    let error = |code: &str, message: String| {
        Value::object([(
            "error",
            Value::object([
                ("code", Value::text(code)),
                ("message", Value::String(message)),
            ]),
        )])
        .encode()
    };
    let http = |status: u64, headers: Vec<(String, String)>, body: String| {
        wire::Answer::Http(wire::Http {
            status,
            headers,
            payload: wire::Payload::Body(body),
        })
    };
    match name {
        "ok" => {
            let reply = fields.first().cloned().unwrap_or(Json::Null);
            let stop = fields.get(1).and_then(Json::as_str).unwrap_or("");
            if stop == "context_exceeded" {
                let failed = Value::object([(
                    "error",
                    Value::object([
                        ("code", Value::text(CONTEXT_EXCEEDED)),
                        ("message", Value::text("context window exceeded")),
                    ]),
                )]);
                let events = vec![event("response.failed", vec![("response", failed)])];
                return wire::Answer::Http(wire::Http {
                    status: HTTP_OK,
                    headers: vec![("content-type".to_owned(), "text/event-stream".to_owned())],
                    payload: wire::Payload::Events(events),
                });
            }
            let (items, end_turn) = items(&reply);
            let mut events: Vec<String> = items
                .into_iter()
                .enumerate()
                .map(|(at, item)| {
                    event(
                        "response.output_item.done",
                        vec![("output_index", Value::number(at as u64)), ("item", item)],
                    )
                })
                .collect();
            let usage = Value::object([
                (
                    "input_tokens",
                    Value::number(number(2).saturating_add(number(4))),
                ),
                (
                    "input_tokens_details",
                    Value::object([("cached_tokens", Value::number(number(4)))]),
                ),
                ("output_tokens", Value::number(number(3))),
            ]);
            let mut response =
                Value::object([("usage", usage), ("end_turn", Value::Bool(end_turn))]);
            let incomplete = INCOMPLETE.iter().find(|(drawn, _)| *drawn == stop);
            let kind = match incomplete {
                Some((_, reason)) => {
                    response.insert(
                        "incomplete_details",
                        Value::object([("reason", Value::text(reason))]),
                    );
                    "response.incomplete"
                }
                None => "response.completed",
            };
            events.push(event(kind, vec![("response", response)]));
            wire::Answer::Http(wire::Http {
                status: HTTP_OK,
                headers: vec![("content-type".to_owned(), "text/event-stream".to_owned())],
                payload: wire::Payload::Events(events),
            })
        }
        "throttled" => http(
            HTTP_TOO_MANY,
            wire::retry_after(number(1)),
            error("rate_limit_exceeded", text(0)),
        ),
        "transient" if number(1) == 0 => wire::Answer::Transport(text(0)),
        "transient" => http(
            HTTP_SERVER_ERROR,
            wire::retry_after(number(1)),
            error("server_error", text(0)),
        ),
        "rejected" => http(
            HTTP_BAD_REQUEST,
            Vec::new(),
            error("invalid_request_error", text(0)),
        ),
        _ => wire::Answer::Stopped,
    }
}
