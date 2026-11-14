use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::json::Value;
use crate::stream::{self, Streamed};
use crate::tools;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const BASE_URL_DEFAULT: &str = "https://api.anthropic.com";
pub const STREAM_IDLE_SECONDS_MAX: u64 = 300;
pub const STREAM_IDLE_BYTES_PER_SECOND: u64 = 1;
pub const CURL_TIMED_OUT: i32 = 28;
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

pub const MILLISECONDS_PER_SECOND: u64 = 1000;
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
                Value::object([("effort", Value::text(EFFORT))]),
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
        if let Some(rest) = self.base_url.strip_prefix("http://") {
            let request = self.scratch.join("request.json");
            let encoded = body.encode();
            std::fs::write(&request, &encoded)
                .map_err(|error| format!("write request: {error}"))?;
            return plain::post(rest, &self.key, &encoded, partial);
        }
        let request = self.scratch.join("request.json");
        let response = partial;
        std::fs::write(&request, body.encode())
            .map_err(|error| format!("write request: {error}"))?;
        let headers = self.scratch.join("headers.txt");
        let _ = std::fs::remove_file(&headers);
        let config = format!(
            "url = \"{}/v1/messages\"\nheader = \"x-api-key: {}\"\nheader = \"anthropic-version: {ANTHROPIC_VERSION}\"\nheader = \"content-type: application/json\"\ndata-binary = \"@{}\"\noutput = \"{}\"\ndump-header = \"{}\"\n",
            self.base_url.trim_end_matches('/'),
            self.key,
            request.display(),
            response.display(),
            headers.display()
        );
        let mut child = Command::new("curl")
            .args([
                "-sS",
                "--speed-limit",
                &STREAM_IDLE_BYTES_PER_SECOND.to_string(),
                "--speed-time",
                &STREAM_IDLE_SECONDS_MAX.to_string(),
                "-w",
                "%{http_code}",
                "-K",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("spawn curl: {error}"))?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(config.as_bytes())
            .map_err(|error| format!("curl stdin: {error}"))?;
        let output = child
            .wait_with_output()
            .map_err(|error| format!("curl: {error}"))?;
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let reason = if code == CURL_TIMED_OUT {
                format!(
                    " (the stream stalled for STREAM_IDLE_SECONDS_MAX, {STREAM_IDLE_SECONDS_MAX} s)"
                )
            } else {
                String::new()
            };
            return Err(format!(
                "curl exited {code}{reason}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let status: u16 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .map_err(|_| "curl printed no HTTP status".to_owned())?;
        let size = std::fs::metadata(response)
            .map_err(|error| format!("read response: {error}"))?
            .len();
        if size > plain::RESPONSE_BYTES_MAX as u64 {
            return Err(format!(
                "response exceeds RESPONSE_BYTES_MAX ({} bytes)",
                plain::RESPONSE_BYTES_MAX
            ));
        }
        let text =
            std::fs::read_to_string(response).map_err(|error| format!("read response: {error}"))?;
        let head = std::fs::read_to_string(&headers).unwrap_or_default();
        Ok((status, text, retry_after_ms(&head)))
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

pub fn retry_after_ms(head: &str) -> u64 {
    let header = |name: &str| {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    if let Some(ms) = header("retry-after-ms").and_then(|value| value.parse::<u64>().ok()) {
        return ms;
    }
    header("retry-after")
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(0, |seconds| seconds.saturating_mul(MILLISECONDS_PER_SECOND))
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

mod plain {
    use std::io::{Read as _, Write as _};
    use std::net::TcpStream;
    use std::path::Path;
    use std::time::Duration;

    pub const RESPONSE_BYTES_MAX: usize = 32 * 1024 * 1024;
    pub const HTTP_PORT: u16 = 80;
    pub const READ_CHUNK_BYTES: usize = 8192;
    const HEADER_END: &[u8] = b"\r\n\r\n";

    pub fn post(
        rest: &str,
        key: &str,
        body: &str,
        partial: &Path,
    ) -> Result<(u16, String, u64), String> {
        let (authority, prefix) = rest
            .split_once('/')
            .map_or((rest, ""), |(authority, path)| (authority, path));
        let path = format!("/{}/v1/messages", prefix.trim_end_matches('/')).replace("//", "/");
        let address = if authority.contains(':') {
            authority.to_owned()
        } else {
            format!("{authority}:{HTTP_PORT}")
        };
        let mut stream =
            TcpStream::connect(&address).map_err(|error| format!("connect {address}: {error}"))?;
        let timeout = Some(Duration::from_secs(super::STREAM_IDLE_SECONDS_MAX));
        stream
            .set_read_timeout(timeout)
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(timeout)
            .map_err(|error| error.to_string())?;
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: {authority}\r\nx-api-key: {key}\r\nanthropic-version: {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            super::ANTHROPIC_VERSION,
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .map_err(|error| format!("send: {error}"))?;
        stream
            .write_all(body.as_bytes())
            .map_err(|error| format!("send: {error}"))?;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(partial)
            .map_err(|error| format!("{}: {error}", partial.display()))?;
        let mut response = Vec::new();
        let mut chunk = [0_u8; READ_CHUNK_BYTES];
        loop {
            let read = stream
                .read(&mut chunk)
                .map_err(|error| format!("receive: {error}"))?;
            if read == 0 {
                break;
            }
            if response.len() + read > RESPONSE_BYTES_MAX {
                return Err(format!(
                    "response exceeds RESPONSE_BYTES_MAX ({RESPONSE_BYTES_MAX} bytes)"
                ));
            }
            response.extend_from_slice(&chunk[..read]);
            file.write_all(&chunk[..read])
                .map_err(|error| format!("{}: {error}", partial.display()))?;
        }
        parse(&response)
    }

    fn parse(response: &[u8]) -> Result<(u16, String, u64), String> {
        let end = response
            .windows(HEADER_END.len())
            .position(|window| window == HEADER_END)
            .ok_or("response has no header end")?;
        let head = String::from_utf8_lossy(&response[..end]).into_owned();
        let mut lines = head.split("\r\n");
        let status: u16 = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| format!("bad status line in {head}"))?;
        let header = |name: &str| {
            head.split("\r\n").skip(1).find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_ascii_lowercase())
            })
        };
        let raw = &response[end + HEADER_END.len()..];
        let body = if header("transfer-encoding").is_some_and(|value| value.contains("chunked")) {
            dechunk(raw)?
        } else if let Some(length) = header("content-length") {
            let length: usize = length
                .parse()
                .map_err(|_| format!("bad content-length {length}"))?;
            raw.get(..length)
                .ok_or("response shorter than its content-length")?
                .to_vec()
        } else {
            raw.to_vec()
        };
        Ok((
            status,
            String::from_utf8(body).map_err(|error| format!("response body: {error}"))?,
            super::retry_after_ms(&head),
        ))
    }

    fn dechunk(mut raw: &[u8]) -> Result<Vec<u8>, String> {
        let mut body = Vec::with_capacity(raw.len());
        loop {
            let line_end = raw
                .windows(2)
                .position(|window| window == b"\r\n")
                .ok_or("chunk without a size line")?;
            let size_text = String::from_utf8_lossy(&raw[..line_end]);
            let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| format!("bad chunk size {size_text}"))?;
            raw = &raw[line_end + 2..];
            if size == 0 {
                return Ok(body);
            }
            let chunk = raw.get(..size).ok_or("chunk shorter than its size")?;
            body.extend_from_slice(chunk);
            raw = raw.get(size + 2..).ok_or("chunk without its line end")?;
        }
    }
}
