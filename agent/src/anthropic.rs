use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::json::Value;
use crate::tools;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const BASE_URL_DEFAULT: &str = "https://api.anthropic.com";
pub const REQUEST_SECONDS_MAX: u64 = 600;
pub const CURL_TIMED_OUT: i32 = 28;
pub const HTTP_OK: u16 = 200;
pub const HTTP_REQUEST_TIMEOUT: u16 = 408;
pub const HTTP_CONFLICT: u16 = 409;
pub const HTTP_TOO_LARGE: u16 = 413;
pub const HTTP_TOO_MANY: u16 = 429;
pub const HTTP_SERVER_ERROR_MIN: u16 = 500;
pub const PROMPT_TOO_LONG: &str = "prompt is too long";

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

impl Usage {
    pub fn context(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

pub enum Answer {
    Reply(Value, Usage),
    Retry(String),
}

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

    fn reply(&mut self, reply: &Value) -> Result<(), String> {
        let (name, payload) = tagged(reply)?;
        match (name, payload) {
            ("answer", Some(Value::Array(fields))) => self.text(
                "assistant",
                fields.first().and_then(Value::as_str).unwrap_or(""),
            ),
            ("truncated", Some(Value::String(text))) => self.text("assistant", text),
            ("calls", Some(Value::Array(fields))) => {
                self.text(
                    "assistant",
                    fields.get(1).and_then(Value::as_str).unwrap_or(""),
                );
                for call in fields
                    .first()
                    .and_then(Value::as_array)
                    .ok_or("calls without a list")?
                {
                    let tool = call
                        .get("tool")
                        .and_then(Value::as_str)
                        .ok_or("a call has no tool")?;
                    let input = call
                        .get("input")
                        .and_then(Value::as_str)
                        .ok_or("a call has no input")?;
                    let input = Value::decode(input).unwrap_or_else(|_| Value::Object(Vec::new()));
                    self.push(
                        "assistant",
                        Value::object([
                            ("type", Value::text("tool_use")),
                            ("id", call.get("id").cloned().ok_or("a call has no id")?),
                            ("name", Value::text(tool)),
                            ("input", tools::without_defaults(tool, input)),
                        ]),
                    );
                }
            }
            ("paused" | "context_full" | "refused" | "rejected", _) => {}
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

pub fn messages(context: &Value) -> Result<Vec<Value>, String> {
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

impl Client {
    pub fn body(&self, context: &Value) -> Result<Value, String> {
        let mut tools = self.tools.clone();
        if let Value::Array(items) = &mut tools
            && let Some(last) = items.last_mut()
        {
            last.insert("cache_control", ephemeral());
        }
        Ok(Value::object([
            ("model", Value::text(&self.model)),
            ("max_tokens", Value::number(self.max_tokens)),
            ("system", self.system.clone()),
            ("tools", tools),
            ("messages", Value::Array(messages(context)?)),
        ]))
    }

    pub fn send(&self, body: &Value) -> Result<(u16, String), String> {
        if let Some(rest) = self.base_url.strip_prefix("http://") {
            let request = self.scratch.join("request.json");
            let encoded = body.encode();
            std::fs::write(&request, &encoded)
                .map_err(|error| format!("write request: {error}"))?;
            return plain::post(rest, &self.key, &encoded);
        }
        let request = self.scratch.join("request.json");
        let response = self.scratch.join("response.json");
        std::fs::write(&request, body.encode())
            .map_err(|error| format!("write request: {error}"))?;
        let _ = std::fs::remove_file(&response);
        let config = format!(
            "url = \"{}/v1/messages\"\nheader = \"x-api-key: {}\"\nheader = \"anthropic-version: {ANTHROPIC_VERSION}\"\nheader = \"content-type: application/json\"\ndata-binary = \"@{}\"\noutput = \"{}\"\n",
            self.base_url.trim_end_matches('/'),
            self.key,
            request.display(),
            response.display()
        );
        let mut child = Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                &REQUEST_SECONDS_MAX.to_string(),
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
                format!(" (no answer within REQUEST_SECONDS_MAX, {REQUEST_SECONDS_MAX} s)")
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
        let text = std::fs::read_to_string(&response)
            .map_err(|error| format!("read response: {error}"))?;
        Ok((status, text))
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

pub fn interpret(status: u16, body: &str) -> Answer {
    if status != HTTP_OK {
        let (kind, text) = error_text(status, body);
        if status == HTTP_TOO_LARGE || text.contains(PROMPT_TOO_LONG) {
            return Answer::Reply(Value::text("context_full"), Usage::default());
        }
        let retryable = matches!(status, HTTP_REQUEST_TIMEOUT | HTTP_CONFLICT | HTTP_TOO_MANY)
            || status >= HTTP_SERVER_ERROR_MIN
            || matches!(
                kind.as_str(),
                "overloaded_error" | "api_error" | "rate_limit_error"
            );
        return if retryable {
            Answer::Retry(text)
        } else {
            Answer::Reply(
                Value::object([("rejected", Value::text(&text))]),
                Usage::default(),
            )
        };
    }
    let value = match Value::decode(body) {
        Ok(value) => value,
        Err(error) => return Answer::Retry(format!("response is not JSON: {error}")),
    };
    let used = usage(&value);
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return Answer::Retry("response has no content".to_owned());
    };
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                let input =
                    tools::with_defaults(name, block.get("input").cloned().unwrap_or(Value::Null));
                calls.push(Value::object([
                    ("id", block.get("id").cloned().unwrap_or(Value::text(""))),
                    ("tool", Value::text(name)),
                    ("input", Value::String(input.encode())),
                ]));
            }
            _ => {}
        }
    }
    let tokens = Value::number(used.context());
    let reply = match value.get("stop_reason").and_then(Value::as_str) {
        Some("tool_use") if !calls.is_empty() => Value::object([(
            "calls",
            Value::Array(vec![Value::Array(calls), Value::String(text), tokens]),
        )]),
        Some("end_turn" | "stop_sequence") if calls.is_empty() => {
            Value::object([("answer", Value::Array(vec![Value::String(text), tokens]))])
        }
        Some("end_turn" | "stop_sequence") => Value::object([(
            "calls",
            Value::Array(vec![Value::Array(calls), Value::String(text), tokens]),
        )]),
        Some("max_tokens") => Value::object([("truncated", Value::String(text))]),
        Some("pause_turn") => Value::text("paused"),
        Some("refusal") => Value::object([(
            "refused",
            Value::String(if text.is_empty() {
                "refusal".to_owned()
            } else {
                text
            }),
        )]),
        Some("model_context_window_exceeded") => Value::text("context_full"),
        other => return Answer::Retry(format!("unexpected stop_reason {other:?}")),
    };
    Answer::Reply(reply, used)
}

mod plain {
    use std::io::{Read as _, Write as _};
    use std::net::TcpStream;
    use std::time::Duration;

    pub const RESPONSE_BYTES_MAX: usize = 32 * 1024 * 1024;
    pub const HTTP_PORT: u16 = 80;
    const HEADER_END: &[u8] = b"\r\n\r\n";

    pub fn post(rest: &str, key: &str, body: &str) -> Result<(u16, String), String> {
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
        let timeout = Some(Duration::from_secs(super::REQUEST_SECONDS_MAX));
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
        let mut response = Vec::new();
        let read = stream
            .take(RESPONSE_BYTES_MAX as u64 + 1)
            .read_to_end(&mut response)
            .map_err(|error| format!("receive: {error}"))?;
        if read > RESPONSE_BYTES_MAX {
            return Err(format!(
                "response exceeds RESPONSE_BYTES_MAX ({RESPONSE_BYTES_MAX} bytes)"
            ));
        }
        parse(&response)
    }

    fn parse(response: &[u8]) -> Result<(u16, String), String> {
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
