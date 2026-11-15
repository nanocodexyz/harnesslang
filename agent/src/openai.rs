use std::path::{Path, PathBuf};

use crate::json::Value;
use crate::tools;
use crate::transport;

pub const BASE_URL_DEFAULT: &str = "https://api.openai.com";
pub const RESPONSES_PATH: &str = "/v1/responses";
pub const EFFORT_SETTING: &str = "effort";
pub const EFFORT_DEFAULT: &str = "medium";
pub const INCLUDE_ENCRYPTED_REASONING: &str = "reasoning.encrypted_content";
pub const VERBOSITY_DEFAULT: &str = "low";
pub const HTTP_OK: u16 = 200;
pub const HTTP_REQUEST_TIMEOUT: u16 = 408;
pub const HTTP_CONFLICT: u16 = 409;
pub const HTTP_TOO_LARGE: u16 = 413;
pub const HTTP_TOO_MANY: u16 = 429;
pub const HTTP_SERVER_ERROR_MIN: u16 = 500;
pub const LONG_CONTEXT_THRESHOLD: u64 = 272_000;
pub const NANO_USD_PER_USD: f64 = 1_000_000_000.0;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub reasoning: u64,
}

#[derive(Clone, Copy)]
pub struct Rates {
    pub input: u64,
    pub cached_input: u64,
    pub output: u64,
}

const SOL_STANDARD: Rates = Rates {
    input: 2_000,
    cached_input: 100,
    output: 10_000,
};
const SOL_LONG_CONTEXT: Rates = Rates {
    input: 4_000,
    cached_input: 200,
    output: 15_000,
};
const TERRA_STANDARD: Rates = Rates {
    input: 1_000,
    cached_input: 50,
    output: 5_000,
};
const TERRA_LONG_CONTEXT: Rates = Rates {
    input: 2_000,
    cached_input: 100,
    output: 7_500,
};
const LUNA_STANDARD: Rates = Rates {
    input: 100,
    cached_input: 10,
    output: 500,
};
const LUNA_LONG_CONTEXT: Rates = Rates {
    input: 200,
    cached_input: 20,
    output: 750,
};
const ASTRA_STANDARD: Rates = Rates {
    input: 10_000,
    cached_input: 1_000,
    output: 50_000,
};
const ASTRA_LONG_CONTEXT: Rates = Rates {
    input: 20_000,
    cached_input: 2_000,
    output: 75_000,
};

pub fn rates(model: &str, input_tokens: u64) -> Option<Rates> {
    let long = input_tokens > LONG_CONTEXT_THRESHOLD;
    let family = ["sol", "terra", "luna", "astra"]
        .into_iter()
        .find(|family| model.ends_with(&format!("-{family}")))?;
    Some(match (family, long) {
        ("sol", false) => SOL_STANDARD,
        ("sol", true) => SOL_LONG_CONTEXT,
        ("terra", false) => TERRA_STANDARD,
        ("terra", true) => TERRA_LONG_CONTEXT,
        ("luna", false) => LUNA_STANDARD,
        ("luna", true) => LUNA_LONG_CONTEXT,
        ("astra", false) => ASTRA_STANDARD,
        _ => ASTRA_LONG_CONTEXT,
    })
}

pub fn cost_nano_usd(model: &str, used: Usage) -> Option<u64> {
    let rates = rates(model, used.input)?;
    let uncached = used.input.saturating_sub(used.cached);
    Some(
        uncached
            .saturating_mul(rates.input)
            .saturating_add(used.cached.saturating_mul(rates.cached_input))
            .saturating_add(used.output.saturating_mul(rates.output)),
    )
}

pub fn verbosity(model: &str) -> bool {
    model.starts_with("gpt-5") || model.starts_with("gpt-6")
}

pub struct Client {
    pub base_url: String,
    pub key: String,
    pub model: String,
    pub instructions: String,
    pub preamble: Vec<Value>,
    pub tools: Value,
    pub custom_tools: Vec<String>,
    pub cache_key: String,
    pub scratch: PathBuf,
}

fn tagged(value: &Value) -> Result<(&str, Option<&Value>), String> {
    match value {
        Value::String(name) => Ok((name, None)),
        Value::Object(entries) if entries.len() == 1 => Ok((&entries[0].0, Some(&entries[0].1))),
        other => Err(format!("expected a variant, found {}", other.encode())),
    }
}

pub fn user_message(text: &str) -> Value {
    Value::object([
        ("type", Value::text("message")),
        ("role", Value::text("user")),
        (
            "content",
            Value::Array(vec![Value::object([
                ("type", Value::text("input_text")),
                ("text", Value::text(text)),
            ])]),
        ),
    ])
}

fn assistant_message(text: &str) -> Value {
    Value::object([
        ("type", Value::text("message")),
        ("role", Value::text("assistant")),
        (
            "content",
            Value::Array(vec![Value::object([
                ("type", Value::text("output_text")),
                ("text", Value::text(text)),
            ])]),
        ),
    ])
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

pub fn input_item(item: &Value) -> Option<Value> {
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

impl Client {
    fn output(&self, call: &Value, text: &str) -> Result<Value, String> {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .ok_or("a call has no id")?;
        let tool = call.get("tool").and_then(Value::as_str).unwrap_or("");
        let kind = if self.custom_tools.iter().any(|custom| custom == tool) {
            "custom_tool_call_output"
        } else {
            "function_call_output"
        };
        Ok(Value::object([
            ("type", Value::text(kind)),
            ("call_id", Value::text(id)),
            ("output", Value::text(text)),
        ]))
    }

    fn reply_items(&self, reply: &Value, items: &mut Vec<Value>) -> Result<(), String> {
        let (name, payload) = tagged(reply)?;
        let fields = payload
            .and_then(Value::as_array)
            .ok_or_else(|| format!("reply {name} has no fields"))?;
        let (words, raw) = match name {
            "message" => (fields.first(), fields.get(1)),
            "calls" => (fields.get(1), fields.get(2)),
            other => return Err(format!("context holds an unknown reply {other}")),
        };
        let raw = raw.and_then(Value::as_str).unwrap_or("");
        if raw.is_empty() {
            let words = words.and_then(Value::as_str).unwrap_or("");
            if !words.is_empty() {
                items.push(assistant_message(words));
            }
            return Ok(());
        }
        let decoded =
            Value::decode(raw).map_err(|error| format!("a reply's items are not JSON: {error}"))?;
        for item in decoded.as_array().ok_or("a reply's items are not a list")? {
            if let Some(kept) = input_item(item) {
                items.push(kept);
            }
        }
        Ok(())
    }

    pub fn input(&self, context: &Value) -> Result<Vec<Value>, String> {
        let mut items = self.preamble.clone();
        for entry in context.as_array().ok_or("the context is not a list")? {
            let (name, payload) = tagged(entry)?;
            let pair = || -> Result<(&Value, &str), String> {
                match payload {
                    Some(Value::Array(fields)) if fields.len() == 2 => {
                        Ok((&fields[0], fields[1].as_str().unwrap_or("")))
                    }
                    _ => Err(format!("context entry {name} lacks its 2 fields")),
                }
            };
            match name {
                "user" => items.push(user_message(
                    payload
                        .and_then(Value::as_str)
                        .ok_or("a user entry has no text")?,
                )),
                "assistant" => self.reply_items(
                    payload.ok_or("an assistant entry has no reply")?,
                    &mut items,
                )?,
                "result" | "error" => {
                    let (call, text) = pair()?;
                    items.push(self.output(call, text)?);
                }
                "unknown" => items.push(self.output(
                    payload.ok_or("an unknown entry has no call")?,
                    tools::unknown_outcome(),
                )?),
                other => {
                    return Err(format!(
                        "context holds an entry {other} this host does not send"
                    ));
                }
            }
        }
        Ok(items)
    }

    pub fn body(&self, context: &Value, effort: Option<&str>) -> Result<Value, String> {
        let reasoning = Value::object([("effort", Value::text(effort.unwrap_or(EFFORT_DEFAULT)))]);
        let mut body = Value::object([
            ("model", Value::text(&self.model)),
            ("instructions", Value::text(&self.instructions)),
            ("input", Value::Array(self.input(context)?)),
            ("tools", self.tools.clone()),
            ("tool_choice", Value::text("auto")),
            ("parallel_tool_calls", Value::Bool(true)),
            ("reasoning", reasoning),
            ("store", Value::Bool(false)),
            ("stream", Value::Bool(true)),
            (
                "include",
                Value::Array(vec![Value::text(INCLUDE_ENCRYPTED_REASONING)]),
            ),
            ("prompt_cache_key", Value::text(&self.cache_key)),
        ]);
        if verbosity(&self.model) {
            body.insert(
                "text",
                Value::object([("verbosity", Value::text(VERBOSITY_DEFAULT))]),
            );
        }
        Ok(body)
    }

    pub fn send(
        &self,
        body: &Value,
        partial: &Path,
        stop: &dyn Fn() -> bool,
    ) -> Result<transport::Sent, String> {
        let encoded = body.encode();
        let headers = [
            ("authorization", format!("Bearer {}", self.key)),
            ("content-type", "application/json".to_owned()),
            ("accept", "text/event-stream".to_owned()),
        ];
        transport::post(&transport::Post {
            base_url: &self.base_url,
            path: RESPONSES_PATH,
            headers: &headers,
            body: &encoded,
            scratch: &self.scratch,
            partial,
            stop,
        })
    }
}

fn attempt(name: &str, fields: Vec<Value>) -> Value {
    Value::object([(name, Value::Array(fields))])
}

pub fn transient(error: &str, retry_after_ms: u64) -> Value {
    attempt(
        "transient",
        vec![Value::text(error), Value::number(retry_after_ms)],
    )
}

pub fn interrupted() -> Value {
    Value::object([("interrupted", Value::Array(Vec::new()))])
}

fn ok(reply: Value, stop: &str, used: Usage) -> Value {
    attempt(
        "ok",
        vec![
            reply,
            Value::text(stop),
            Value::number(used.input.saturating_sub(used.cached)),
            Value::number(used.output),
            Value::number(used.cached),
            Value::number(0),
        ],
    )
}

fn empty_message() -> Value {
    Value::object([(
        "message",
        Value::Array(vec![Value::text(""), Value::text(""), Value::Bool(true)]),
    )])
}

fn hinted_ms(message: &str) -> u64 {
    let lower = message.to_ascii_lowercase();
    let Some(rest) = lower.split(RETRY_HINT).nth(1) else {
        return 0;
    };
    let rest = rest.trim_start();
    let number: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let Ok(value) = number.parse::<f64>() else {
        return 0;
    };
    let unit = rest[number.len()..].trim_start();
    let ms = if unit.starts_with("ms") {
        value
    } else if unit.starts_with('s') {
        value * transport::MILLISECONDS_PER_SECOND as f64
    } else {
        return 0;
    };
    ms.ceil() as u64
}

pub fn classify(status: Option<u16>, code: &str, message: &str, retry_after_ms: u64) -> Value {
    let text = match status {
        Some(status) => format!("HTTP {status} {code}: {message}"),
        None => format!("response failed {code}: {message}"),
    };
    if code == CONTEXT_EXCEEDED || status == Some(HTTP_TOO_LARGE) {
        return ok(empty_message(), "context_exceeded", Usage::default());
    }
    if QUOTA_CODES.contains(&code) || REJECTED_CODES.contains(&code) {
        return attempt("rejected", vec![Value::text(&text)]);
    }
    let after = if retry_after_ms > 0 {
        retry_after_ms
    } else {
        hinted_ms(message)
    };
    if THROTTLE_CODES.contains(&code) || status == Some(HTTP_TOO_MANY) {
        return attempt("throttled", vec![Value::text(&text), Value::number(after)]);
    }
    match status {
        Some(status)
            if matches!(status, HTTP_REQUEST_TIMEOUT | HTTP_CONFLICT)
                || status >= HTTP_SERVER_ERROR_MIN =>
        {
            transient(&text, after)
        }
        Some(_) => attempt("rejected", vec![Value::text(&text)]),
        None => transient(&text, after),
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

pub fn usage_of(response: &Value) -> Usage {
    let usage = response.get("usage");
    let field = |path: &[&str]| {
        let mut at = usage;
        for key in path {
            at = at.and_then(|value| value.get(key));
        }
        at.and_then(Value::as_u64).unwrap_or(0)
    };
    Usage {
        input: field(&["input_tokens"]),
        cached: field(&["input_tokens_details", "cached_tokens"]),
        output: field(&["output_tokens"]),
        reasoning: field(&["output_tokens_details", "reasoning_tokens"]),
    }
}

fn call_input(arguments: &str) -> String {
    match Value::decode(arguments) {
        Ok(Value::Object(entries)) => Value::Object(
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

pub fn reply_of(
    items: &[Value],
    end_turn: bool,
    incomplete: Option<&str>,
) -> (Value, &'static str) {
    let mut words = String::new();
    let mut refused = false;
    let mut calls = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item.get("content").and_then(Value::as_array).unwrap_or(&[]) {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => {
                            words.push_str(part.get("text").and_then(Value::as_str).unwrap_or(""))
                        }
                        Some("refusal") => {
                            refused = true;
                            words
                                .push_str(part.get("refusal").and_then(Value::as_str).unwrap_or(""))
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
                calls.push(Value::object([
                    (
                        "id",
                        item.get("call_id").cloned().unwrap_or(Value::text("")),
                    ),
                    ("tool", item.get("name").cloned().unwrap_or(Value::text(""))),
                    ("input", Value::String(call_input(arguments))),
                ]));
            }
            Some("custom_tool_call") => {
                let input = item.get("input").and_then(Value::as_str).unwrap_or("");
                calls.push(Value::object([
                    (
                        "id",
                        item.get("call_id").cloned().unwrap_or(Value::text("")),
                    ),
                    ("tool", item.get("name").cloned().unwrap_or(Value::text(""))),
                    (
                        "input",
                        Value::String(Value::object([("input", Value::text(input))]).encode()),
                    ),
                ]));
            }
            _ => {}
        }
    }
    let kept: Vec<Value> = items.iter().filter_map(input_item).collect();
    let raw = Value::String(Value::Array(kept).encode());
    let stop = match incomplete {
        Some("max_output_tokens") => "max_tokens",
        Some("content_filter") => "refusal",
        Some(_) => "pause",
        None if refused => "refusal",
        None if calls.is_empty() => "end",
        None => "tool_use",
    };
    let reply = if calls.is_empty() {
        Value::object([(
            "message",
            Value::Array(vec![Value::String(words), raw, Value::Bool(end_turn)]),
        )])
    } else {
        Value::object([(
            "calls",
            Value::Array(vec![Value::Array(calls), Value::String(words), raw]),
        )])
    };
    (reply, stop)
}

struct Assembly {
    items: Vec<(u64, Value)>,
    terminal: Option<Value>,
    failed: Option<Value>,
}

fn data_lines(stream: &str) -> impl Iterator<Item = &str> {
    stream
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty() && *data != "[DONE]")
}

fn assemble(stream: &str) -> Result<Assembly, String> {
    let mut assembly = Assembly {
        items: Vec::new(),
        terminal: None,
        failed: None,
    };
    for data in data_lines(stream) {
        let event =
            Value::decode(data).map_err(|error| format!("stream event is not JSON: {error}"))?;
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

pub fn interpret(status: u16, body: &str, retry_after_ms: u64) -> (Value, Usage) {
    if status != HTTP_OK {
        let decoded = Value::decode(body).ok();
        let (code, message) = error_fields(decoded.as_ref().and_then(|value| value.get("error")));
        let message = if message.is_empty() {
            body.to_owned()
        } else {
            message
        };
        return (
            classify(Some(status), &code, &message, retry_after_ms),
            Usage::default(),
        );
    }
    let assembly = match assemble(body) {
        Ok(assembly) => assembly,
        Err(error) => return (transient(&error, 0), Usage::default()),
    };
    if let Some(error) = assembly.failed {
        let (code, message) = error_fields(Some(&error));
        return (classify(None, &code, &message, 0), Usage::default());
    }
    let Some(terminal) = assembly.terminal else {
        return (
            transient("stream closed before response.completed", 0),
            Usage::default(),
        );
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
    (ok(reply, stop, used), used)
}
