use std::path::{Path, PathBuf};

use crate::json::Value;
use crate::stream::{self, Streamed};
use crate::tools;
use crate::transport;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const BASE_URL_DEFAULT: &str = "https://api.anthropic.com";
pub const MESSAGES_PATH: &str = "/v1/messages";
pub const MODE_TEXT_ONLY: &str = "text_only";
pub const MODE_REFRESH: &str = "refresh";
pub const REFRESH_MAX_TOKENS: u64 = 0;
pub const HTTP_OK: u16 = 200;
pub const HTTP_REQUEST_TIMEOUT: u16 = 408;
pub const HTTP_CONFLICT: u16 = 409;
pub const HTTP_TOO_LARGE: u16 = 413;
pub const HTTP_TOO_MANY: u16 = 429;
pub const HTTP_SERVER_ERROR_MIN: u16 = 500;
pub const EFFORT: &str = "high";
pub const EFFORT_SETTING: &str = "effort";
pub const PROMPT_TOO_LONG: &str = "prompt is too long";

pub fn adaptive_thinking(model: &str) -> bool {
    matches!(
        model,
        "claude-opus-5-5"
            | "claude-sonnet-5-5"
            | "claude-haiku-5-5"
            | "claude-fable-5-5"
            | "claude-fable-5-2"
            | "claude-mythos-5-2"
            | "claude-fable-5-1"
            | "claude-mythos-5-1"
            | "claude-opus-5"
            | "claude-sonnet-5"
            | "claude-opus-4-6"
            | "claude-sonnet-4-6"
    )
}

pub fn model_max_tokens(model: &str) -> Option<u64> {
    match model {
        "claude-opus-5-5" | "claude-fable-5-5" | "claude-fable-5-2" | "claude-mythos-5-2"
        | "claude-fable-5-1" | "claude-mythos-5-1" | "claude-sonnet-5-5" | "claude-haiku-5-5"
        | "claude-opus-5" | "claude-sonnet-5" | "claude-opus-4-6" | "claude-sonnet-4-6" => {
            Some(128_000)
        }
        "claude-haiku-4-5"
        | "claude-haiku-4-5-20251001"
        | "claude-sonnet-4-5"
        | "claude-sonnet-4-5-20250929"
        | "claude-opus-4-5"
        | "claude-opus-4-5-20251101" => Some(64_000),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

pub const HTTP_OVERLOADED: u16 = 529;

pub struct Client {
    pub base_url: String,
    pub key: String,
    pub model: String,
    pub max_tokens: u64,
    pub system: Value,
    pub tools: Value,
    pub scratch: PathBuf,
}

fn ephemeral() -> Value {
    Value::object([("type", Value::text("ephemeral"))])
}

fn text_block(text: &str) -> Value {
    Value::object([("type", Value::text("text")), ("text", Value::text(text))])
}

struct Transcript {
    messages: Vec<(&'static str, Vec<Value>)>,
}

impl Transcript {
    fn push(&mut self, role: &'static str, block: Value) {
        match self.messages.last_mut() {
            Some((last, blocks)) if *last == role => blocks.push(block),
            _ => self.messages.push((role, vec![block])),
        }
    }

    fn text(&mut self, role: &'static str, text: &str) {
        if !text.is_empty() {
            self.push(role, text_block(text));
        }
    }

    fn result(&mut self, call: &Value, content: &str, error: bool) -> Result<(), String> {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .ok_or("a call has no id")?;
        let mut block = Value::object([
            ("type", Value::text("tool_result")),
            ("tool_use_id", Value::text(id)),
        ]);
        if !content.is_empty() {
            block.insert("content", Value::text(content));
        }
        if error {
            block.insert("is_error", Value::Bool(true));
        }
        self.push("user", block);
        Ok(())
    }

    fn attached(&mut self, call: &Value, media_type: &str, data: &str) -> Result<(), String> {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .ok_or("a call has no id")?;
        let source = Value::object([
            ("type", Value::text("base64")),
            ("media_type", Value::text(media_type)),
            ("data", Value::text(data)),
        ]);
        let image = Value::object([("type", Value::text("image")), ("source", source)]);
        let block = Value::object([
            ("type", Value::text("tool_result")),
            ("tool_use_id", Value::text(id)),
            ("content", Value::Array(vec![image])),
        ]);
        self.push("user", block);
        Ok(())
    }

    fn content(&mut self, content: &str) -> Result<(), String> {
        let blocks = Value::decode(content)
            .map_err(|error| format!("a reply's content is not JSON: {error}"))?;
        for block in blocks.as_array().ok_or("a reply's content is not a list")? {
            let empty_text = block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty);
            if !empty_text {
                self.push("assistant", block.clone());
            }
        }
        Ok(())
    }

    fn reply(&mut self, reply: &Value) -> Result<(), String> {
        let (name, payload) = tagged(reply)?;
        match (name, payload) {
            ("answer", Some(Value::Array(fields))) => {
                let content = fields
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or("an answer has no content")?;
                if content.is_empty() {
                    self.text(
                        "assistant",
                        fields.first().and_then(Value::as_str).unwrap_or(""),
                    );
                } else {
                    self.content(content)?;
                }
            }
            ("calls", Some(Value::Array(fields))) => self.content(
                fields
                    .get(2)
                    .and_then(Value::as_str)
                    .ok_or("calls have no content")?,
            )?,
            (other, _) => return Err(format!("context holds an unknown reply {other}")),
        }
        Ok(())
    }
}

fn tagged(value: &Value) -> Result<(&str, Option<&Value>), String> {
    match value {
        Value::String(name) => Ok((name, None)),
        Value::Object(entries) if entries.len() == 1 => Ok((&entries[0].0, Some(&entries[0].1))),
        other => Err(format!("expected a variant, found {}", other.encode())),
    }
}

pub type Attached<'a> = dyn Fn(&str) -> Option<(String, String)> + 'a;

pub fn messages(context: &Value, attached: &Attached<'_>) -> Result<Vec<Value>, String> {
    let mut transcript = Transcript {
        messages: Vec::new(),
    };
    for entry in context.as_array().ok_or("the context is not a list")? {
        let (name, payload) = tagged(entry)?;
        let fields = |count: usize| -> Result<&[Value], String> {
            match payload {
                Some(Value::Array(fields)) if fields.len() == count => Ok(fields),
                _ => Err(format!("context entry {name} lacks its {count} fields")),
            }
        };
        match name {
            "user" => transcript.text(
                "user",
                payload
                    .and_then(Value::as_str)
                    .ok_or("a user entry has no text")?,
            ),
            "assistant" => transcript.reply(payload.ok_or("an assistant entry has no reply")?)?,
            "result" => {
                let fields = fields(2)?;
                transcript.result(&fields[0], fields[1].as_str().unwrap_or(""), false)?;
            }
            "error" => {
                let fields = fields(2)?;
                transcript.result(&fields[0], fields[1].as_str().unwrap_or(""), true)?;
            }
            "unknown" => transcript.result(
                payload.ok_or("an unknown entry has no call")?,
                tools::unknown_outcome(),
                true,
            )?,
            "notice" => transcript.text(
                "user",
                payload
                    .and_then(Value::as_str)
                    .ok_or("a notice entry has no text")?,
            ),
            "attached" => {
                let fields = fields(2)?;
                let digest = fields[1]
                    .get("blob")
                    .and_then(Value::as_str)
                    .ok_or("an attached entry has no blob digest")?;
                let (media_type, data) = attached(digest).ok_or_else(|| {
                    format!("context holds blob {digest}, which the session does not hold")
                })?;
                transcript.attached(&fields[0], &media_type, &data)?;
            }
            other => return Err(format!("context holds an unknown entry {other}")),
        }
    }
    let mut messages: Vec<Value> = transcript
        .messages
        .into_iter()
        .map(|(role, blocks)| {
            Value::object([
                ("role", Value::text(role)),
                ("content", Value::Array(blocks)),
            ])
        })
        .collect();
    if let Some(Value::Object(entries)) = messages.last_mut()
        && let Some((_, Value::Array(blocks))) =
            entries.iter_mut().find(|(key, _)| key == "content")
        && let Some(block) = blocks.last_mut()
    {
        block.insert("cache_control", ephemeral());
    }
    Ok(messages)
}

fn kept_request(context: &Value) -> Result<Value, String> {
    let entries = context.as_array().ok_or("the context is not a list")?;
    let last = entries
        .iter()
        .rposition(|entry| entry.get("assistant").is_some())
        .ok_or("a refresh needs a context that holds a reply")?;
    Ok(Value::Array(entries[..last].to_vec()))
}

impl Client {
    pub fn body(
        &self,
        context: &Value,
        mode: Option<&str>,
        effort: Option<&str>,
        attached: &Attached<'_>,
    ) -> Result<Value, String> {
        let mut tools = self.tools.clone();
        if let Value::Array(items) = &mut tools
            && let Some(last) = items.last_mut()
        {
            last.insert("cache_control", ephemeral());
        }
        let refresh = mode == Some(MODE_REFRESH);
        let kept = if refresh {
            kept_request(context)?
        } else {
            context.clone()
        };
        let mut body = Value::object([
            ("model", Value::text(&self.model)),
            (
                "max_tokens",
                Value::number(if refresh {
                    REFRESH_MAX_TOKENS
                } else {
                    self.max_tokens
                }),
            ),
            ("system", self.system.clone()),
            ("tools", tools),
            ("messages", Value::Array(messages(&kept, attached)?)),
            ("stream", Value::Bool(!refresh)),
        ]);
        match mode {
            None | Some(MODE_REFRESH) => {}
            Some(MODE_TEXT_ONLY) => {
                body.insert(
                    "tool_choice",
                    Value::object([("type", Value::text("none"))]),
                );
            }
            Some(other) => {
                return Err(format!(
                    "the harness asked for mode {other}, which this host does not know"
                ));
            }
        }
        if adaptive_thinking(&self.model) {
            body.insert(
                "thinking",
                Value::object([
                    ("type", Value::text("adaptive")),
                    ("display", Value::text("summarized")),
                ]),
            );
            body.insert(
                "output_config",
                Value::object([(EFFORT_SETTING, Value::text(effort.unwrap_or(EFFORT)))]),
            );
        } else {
            body.insert(
                "thinking",
                Value::object([("type", Value::text("disabled"))]),
            );
        }
        Ok(body)
    }

    pub fn send(&self, body: &Value, partial: &Path) -> Result<(u16, String, u64), String> {
        let encoded = body.encode();
        let headers = [
            ("x-api-key", self.key.clone()),
            ("anthropic-version", ANTHROPIC_VERSION.to_owned()),
            ("content-type", "application/json".to_owned()),
        ];
        let sent = transport::post(&transport::Post {
            base_url: &self.base_url,
            path: MESSAGES_PATH,
            headers: &headers,
            body: &encoded,
            scratch: &self.scratch,
            partial,
            stop: &|| false,
        })?;
        match sent {
            transport::Sent::Done(status, text, retry_after_ms) => {
                Ok((status, text, retry_after_ms))
            }
            transport::Sent::Stopped => Err("the request was stopped".to_owned()),
        }
    }
}

fn usage(body: &Value) -> Usage {
    let field = |name: &str| {
        body.get("usage")
            .and_then(|usage| usage.get(name))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    Usage {
        input: field("input_tokens"),
        output: field("output_tokens"),
        cache_read: field("cache_read_input_tokens"),
        cache_write: field("cache_creation_input_tokens"),
    }
}

fn error_text(status: u16, body: &str) -> (String, String) {
    match Value::decode(body) {
        Ok(value) => {
            let error = value.get("error");
            let kind = error
                .and_then(|error| error.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("error")
                .to_owned();
            let message = error
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or(body)
                .to_owned();
            let text = format!("HTTP {status} {kind}: {message}");
            (kind, text)
        }
        Err(_) => ("error".to_owned(), format!("HTTP {status}: {body}")),
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

fn ok(reply: Value, stop: &str, used: Usage) -> Value {
    attempt(
        "ok",
        vec![
            reply,
            Value::text(stop),
            Value::number(used.input),
            Value::number(used.output),
            Value::number(used.cache_read),
            Value::number(used.cache_write),
        ],
    )
}

fn error_attempt(status: Option<u16>, kind: &str, text: &str, retry_after_ms: u64) -> Value {
    if status == Some(HTTP_TOO_LARGE) || text.contains(PROMPT_TOO_LONG) {
        let empty = Value::object([(
            "answer",
            Value::Array(vec![Value::text(""), Value::text("")]),
        )]);
        return ok(empty, "context_exceeded", Usage::default());
    }
    let throttled = status.is_some_and(|status| matches!(status, HTTP_TOO_MANY | HTTP_OVERLOADED))
        || matches!(kind, "overloaded_error" | "rate_limit_error");
    let retryable = status.is_some_and(|status| {
        matches!(status, HTTP_REQUEST_TIMEOUT | HTTP_CONFLICT) || status >= HTTP_SERVER_ERROR_MIN
    }) || kind == "api_error";
    if throttled {
        attempt(
            "throttled",
            vec![Value::text(text), Value::number(retry_after_ms)],
        )
    } else if retryable {
        transient(text, retry_after_ms)
    } else {
        attempt("rejected", vec![Value::text(text)])
    }
}

pub fn interpret_stream(status: u16, body: &str, retry_after_ms: u64) -> (Value, Usage) {
    if status != HTTP_OK {
        return interpret(status, body, retry_after_ms);
    }
    match stream::assemble(body) {
        Ok(Streamed::Message(message)) => interpret(status, &message.encode(), retry_after_ms),
        Ok(Streamed::Error { kind, message }) => {
            let text = format!("stream error {kind}: {message}");
            (error_attempt(None, &kind, &text, 0), Usage::default())
        }
        Err(error) => (transient(&error, 0), Usage::default()),
    }
}

pub fn interpret(status: u16, body: &str, retry_after_ms: u64) -> (Value, Usage) {
    if status != HTTP_OK {
        let (kind, text) = error_text(status, body);
        return (
            error_attempt(Some(status), &kind, &text, retry_after_ms),
            Usage::default(),
        );
    }
    let value = match Value::decode(body) {
        Ok(value) => value,
        Err(error) => {
            return (
                transient(&format!("response is not JSON: {error}"), 0),
                Usage::default(),
            );
        }
    };
    let used = usage(&value);
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return (transient("response has no content", 0), used);
    };
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                calls.push(Value::object([
                    ("id", block.get("id").cloned().unwrap_or(Value::text(""))),
                    ("tool", Value::text(name)),
                    ("input", Value::String(input.encode())),
                ]));
            }
            _ => {}
        }
    }
    let raw = Value::String(Value::Array(content.to_vec()).encode());
    let stop = match value.get("stop_reason").and_then(Value::as_str) {
        Some("end_turn" | "stop_sequence") => "end",
        Some("tool_use") => "tool_use",
        Some("max_tokens") => "max_tokens",
        Some("pause_turn") => "pause",
        Some("refusal") => "refusal",
        Some("model_context_window_exceeded") => "context_exceeded",
        other => {
            return (
                transient(&format!("unexpected stop_reason {other:?}"), 0),
                used,
            );
        }
    };
    let reply = if calls.is_empty() {
        Value::object([("answer", Value::Array(vec![Value::String(text), raw]))])
    } else {
        Value::object([(
            "calls",
            Value::Array(vec![Value::Array(calls), Value::String(text), raw]),
        )])
    };
    (ok(reply, stop, used), used)
}
