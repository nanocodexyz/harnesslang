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
pub const HTTP_OVERLOADED: u64 = 529;
pub const PROMPT_TOO_LONG: &str = "prompt is too long";
pub const STOPS: [(&str, &str); 7] = [
    ("end", "end_turn"),
    ("end", "stop_sequence"),
    ("tool_use", "tool_use"),
    ("max_tokens", "max_tokens"),
    ("pause", "pause_turn"),
    ("refusal", "refusal"),
    ("context_exceeded", "model_context_window_exceeded"),
];

fn ephemeral() -> Value {
    Value::object([("type", Value::text("ephemeral"))])
}

fn text_block(text: &str) -> Value {
    Value::object([("type", Value::text("text")), ("text", Value::text(text))])
}

fn tool_use(call: &Value) -> Result<Value, String> {
    let input = call
        .get("input")
        .and_then(Value::as_str)
        .ok_or("a call has no input")?;
    let input = Value::decode(input).ok_or("a call's input is not JSON")?;
    Ok(Value::object([
        ("type", Value::text("tool_use")),
        ("id", call.get("id").cloned().ok_or("a call has no id")?),
        (
            "name",
            call.get("tool").cloned().ok_or("a call has no tool")?,
        ),
        ("input", input),
    ]))
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
        let blocks = Value::decode(content).ok_or("a reply's content is not JSON")?;
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
        let (name, payload) = wire::tagged(reply).ok_or("an assistant entry has no reply")?;
        let fields = payload.and_then(Value::as_array).unwrap_or(&[]);
        let text = |at: usize| fields.get(at).and_then(Value::as_str);
        match name {
            "answer" if fields.len() == 2 => {
                let content = text(1).ok_or("an answer has no content")?;
                if content.is_empty() {
                    self.text("assistant", text(0).ok_or("an answer has no words")?);
                    return Ok(());
                }
                self.content(content)
            }
            "calls" if fields.len() == 3 => {
                let content = text(2).ok_or("calls have no content")?;
                if !content.is_empty() {
                    return self.content(content);
                }
                self.text("assistant", text(1).ok_or("calls have no words")?);
                let calls = fields[0].as_array().ok_or("calls have no list")?;
                for call in calls {
                    let block = tool_use(call)?;
                    self.push("assistant", block);
                }
                Ok(())
            }
            _ => Err("context holds a reply this wire does not know".to_owned()),
        }
    }
}

pub fn messages(context: &Value, attached: &Attached<'_>) -> Result<Vec<Value>, String> {
    let mut transcript = Transcript {
        messages: Vec::new(),
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
            "user" => transcript.text("user", text().ok_or("a user entry has no text")?),
            "assistant" => transcript.reply(payload.ok_or("an assistant entry has no reply")?)?,
            "result" => {
                let (call, output) = pair()?;
                transcript.result(call, output.as_str().unwrap_or(""), false)?;
            }
            "error" => {
                let (call, output) = pair()?;
                transcript.result(call, output.as_str().unwrap_or(""), true)?;
            }
            "unknown" => transcript.result(
                payload.ok_or("an unknown entry has no call")?,
                wire::UNKNOWN_OUTCOME,
                true,
            )?,
            "notice" => transcript.text("user", text().ok_or("a notice entry has no text")?),
            "attached" => {
                let (call, blob) = pair()?;
                let digest = blob
                    .get("blob")
                    .and_then(Value::as_str)
                    .ok_or("an attached entry has no blob digest")?;
                let (media_type, data) = attached(digest)
                    .ok_or("context holds a blob which the session does not hold")?;
                transcript.attached(call, &media_type, &data)?;
            }
            _ => return Err("context holds an entry this wire does not know".to_owned()),
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
        && let Some(block @ Value::Object(_)) = blocks.last_mut()
    {
        block.insert("cache_control", ephemeral());
    }
    Ok(messages)
}

pub fn kept(context: &Value) -> Result<Value, String> {
    let entries = context.as_array().ok_or("the context is not a list")?;
    let last = entries
        .iter()
        .rposition(|entry| entry.get("assistant").is_some())
        .ok_or("a refresh needs a context that holds a reply")?;
    Ok(Value::Array(entries[..last].to_vec()))
}

pub fn render(
    context: &Value,
    refresh: bool,
    attached: &Attached<'_>,
) -> Result<Vec<Value>, String> {
    if refresh {
        return messages(&kept(context)?, attached);
    }
    messages(context, attached)
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

fn error_text(status: u64, body: &str) -> (String, String) {
    let Some(value) = Value::decode(body) else {
        return ("error".to_owned(), format!("HTTP {status}: {body}"));
    };
    let error = value.get("error");
    let kind = error
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("error")
        .to_owned();
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(body);
    let text = format!("HTTP {status} {kind}: {message}");
    (kind, text)
}

fn error_attempt(status: Option<u64>, kind: &str, text: &str, retry_after_ms: u64) -> Json {
    if status == Some(HTTP_TOO_LARGE) || text.contains(PROMPT_TOO_LONG) {
        let empty = wire::tagged_json("answer", vec![Json::text(""), Json::text("")]);
        return wire::ok(empty, "context_exceeded", Usage::default());
    }
    let throttled = status.is_some_and(|status| matches!(status, HTTP_TOO_MANY | HTTP_OVERLOADED))
        || matches!(kind, "overloaded_error" | "rate_limit_error");
    let retryable = status.is_some_and(|status| {
        matches!(status, HTTP_REQUEST_TIMEOUT | HTTP_CONFLICT) || status >= HTTP_SERVER_ERROR
    }) || kind == "api_error";
    if throttled {
        wire::throttled(text, retry_after_ms)
    } else if retryable {
        wire::transient(text, retry_after_ms)
    } else {
        wire::rejected(text)
    }
}

struct Message {
    head: Value,
    blocks: Vec<Value>,
    inputs: Vec<String>,
    stop_reason: Option<Value>,
    usage: Vec<(String, Value)>,
}

fn field<'v>(value: &'v Value, key: &str, what: &str) -> Result<&'v Value, String> {
    value
        .get(key)
        .ok_or_else(|| format!("stream {what} event has no {key}"))
}

fn index(event: &Value, blocks: usize, what: &str) -> Result<usize, String> {
    let index = field(event, "index", what)?
        .as_u64()
        .ok_or_else(|| format!("stream {what} event's index is not a number"))?;
    usize::try_from(index)
        .ok()
        .filter(|index| *index < blocks)
        .ok_or_else(|| format!("stream {what} event names block {index}, which has not started"))
}

fn append(block: &mut Value, key: &str, delta: &str) {
    let mut text = block
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    text.push_str(delta);
    block.insert(key, Value::String(text));
}

fn merge_usage(usage: &mut Vec<(String, Value)>, update: Option<&Value>) {
    let Some(Value::Object(entries)) = update else {
        return;
    };
    for (key, value) in entries {
        match usage.iter_mut().find(|(existing, _)| existing == key) {
            Some((_, slot)) => *slot = value.clone(),
            None => usage.push((key.clone(), value.clone())),
        }
    }
}

impl Message {
    fn delta(&mut self, event: &Value) -> Result<(), String> {
        let at = index(event, self.blocks.len(), "content_block_delta")?;
        let delta = field(event, "delta", "content_block_delta")?;
        let text = |key: &str| {
            delta
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("stream delta has no {key}"))
        };
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => append(&mut self.blocks[at], "text", text("text")?),
            Some("thinking_delta") => append(&mut self.blocks[at], "thinking", text("thinking")?),
            Some("signature_delta") => {
                self.blocks[at].insert("signature", Value::text(text("signature")?));
            }
            Some("input_json_delta") => self.inputs[at].push_str(text("partial_json")?),
            Some("citations_delta") => {}
            _ => return Err("stream delta of unknown type".to_owned()),
        }
        Ok(())
    }

    fn stop(&mut self, event: &Value) -> Result<(), String> {
        let at = index(event, self.blocks.len(), "content_block_stop")?;
        if self.inputs[at].is_empty() {
            return Ok(());
        }
        let input = Value::decode(&self.inputs[at]).ok_or("stream tool input is not JSON")?;
        self.blocks[at].insert("input", input);
        Ok(())
    }

    fn finish(self) -> Value {
        let mut message = self.head;
        message.insert("content", Value::Array(self.blocks));
        message.insert("stop_reason", self.stop_reason.unwrap_or(Value::Null));
        message.insert("usage", Value::Object(self.usage));
        message
    }
}

enum Streamed {
    Message(Value),
    Error(String, String),
}

fn assemble(events: &[String]) -> Result<Streamed, String> {
    let mut message: Option<Message> = None;
    for data in events
        .iter()
        .map(|data| data.trim())
        .filter(|data| !data.is_empty())
    {
        let event = Value::decode(data).ok_or("stream event is not JSON")?;
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "error" {
            let error = event.get("error");
            let text = |key: &str| {
                error
                    .and_then(|error| error.get(key))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            return Ok(Streamed::Error(text("type"), text("message")));
        }
        if kind == "ping" {
            continue;
        }
        if kind == "message_start" {
            let head = field(&event, "message", "message_start")?.clone();
            if !matches!(head, Value::Object(_)) {
                return Err("stream message_start event's message is not an object".to_owned());
            }
            let mut usage = Vec::new();
            merge_usage(&mut usage, head.get("usage"));
            message = Some(Message {
                head,
                blocks: Vec::new(),
                inputs: Vec::new(),
                stop_reason: None,
                usage,
            });
            continue;
        }
        let open = message
            .as_mut()
            .ok_or_else(|| format!("stream {kind} event before message_start"))?;
        match kind {
            "content_block_start" => {
                let block = field(&event, "content_block", kind)?.clone();
                if !matches!(block, Value::Object(_)) {
                    return Err(
                        "stream content_block_start event's block is not an object".to_owned()
                    );
                }
                let at = field(&event, "index", kind)?.as_u64();
                if at != Some(open.blocks.len() as u64) {
                    return Err(format!(
                        "stream starts a block after {} blocks",
                        open.blocks.len()
                    ));
                }
                open.blocks.push(block);
                open.inputs.push(String::new());
            }
            "content_block_delta" => open.delta(&event)?,
            "content_block_stop" => open.stop(&event)?,
            "message_delta" => {
                if let Some(reason) = event
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                {
                    open.stop_reason = Some(reason.clone());
                }
                merge_usage(&mut open.usage, event.get("usage"));
            }
            "message_stop" => {
                let done = message.take().expect("a message is open");
                return Ok(Streamed::Message(done.finish()));
            }
            other => return Err(format!("stream event of unknown type {other}")),
        }
    }
    Err("the stream ended before message_stop".to_owned())
}

fn interpret(value: &Value) -> Json {
    let used = usage(value);
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return wire::transient("response has no content", 0);
    };
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                calls.push(wire::call(id, name, &input.encode()));
            }
            _ => {}
        }
    }
    let raw = Json::String(Value::Array(content.to_vec()).encode());
    let reason = value.get("stop_reason").and_then(Value::as_str);
    let Some(stop) = reason.and_then(|reason| {
        STOPS
            .iter()
            .find(|(_, wired)| *wired == reason)
            .map(|(stop, _)| *stop)
    }) else {
        return wire::transient(
            &format!("unexpected stop_reason {}", reason.unwrap_or("none")),
            0,
        );
    };
    let reply = if calls.is_empty() {
        wire::tagged_json("answer", vec![Json::String(text), raw])
    } else {
        wire::tagged_json("calls", vec![Json::Array(calls), Json::String(text), raw])
    };
    wire::ok(reply, stop, used)
}

pub fn decode(http: &wire::Http) -> Json {
    let retry_after_ms = wire::retry_after_ms(&http.headers);
    match (&http.payload, http.status) {
        (wire::Payload::Events(events), HTTP_OK) => match assemble(events) {
            Ok(Streamed::Message(message)) => interpret(&message),
            Ok(Streamed::Error(kind, message)) => {
                let text = format!("stream error {kind}: {message}");
                error_attempt(None, &kind, &text, 0)
            }
            Err(error) => wire::transient(&error, 0),
        },
        (wire::Payload::Body(body), HTTP_OK) => match Value::decode(body) {
            Some(value) => interpret(&value),
            None => wire::transient("response is not JSON", 0),
        },
        (wire::Payload::Events(_), status) => {
            let (kind, text) = error_text(status, "");
            error_attempt(Some(status), &kind, &text, retry_after_ms)
        }
        (wire::Payload::Body(body), status) => {
            let (kind, text) = error_text(status, body);
            error_attempt(Some(status), &kind, &text, retry_after_ms)
        }
    }
}

fn blocks(reply: &Json) -> Vec<Value> {
    let Some((name, Json::Array(fields))) = reply.tagged() else {
        return Vec::new();
    };
    let text = |at: usize| fields.get(at).and_then(Json::as_str).unwrap_or("");
    let content = if name == "calls" { text(2) } else { text(1) };
    if let Some(Value::Array(blocks)) = Value::decode(content) {
        return blocks;
    }
    let words = if name == "calls" { text(1) } else { text(0) };
    let mut out = Vec::new();
    if !words.is_empty() {
        out.push(text_block(words));
    }
    if name == "calls"
        && let Some(Json::Array(calls)) = fields.first()
    {
        for call in calls {
            let field = |key: &str| call.get(key).and_then(Json::as_str).unwrap_or("");
            let input = Value::decode(field("input")).unwrap_or(Value::Object(Vec::new()));
            out.push(Value::object([
                ("type", Value::text("tool_use")),
                ("id", Value::text(field("id"))),
                ("name", Value::text(field("tool"))),
                ("input", input),
            ]));
        }
    }
    out
}

fn event(kind: &str, mut fields: Vec<(&str, Value)>) -> Value {
    fields.insert(0, ("type", Value::text(kind)));
    Value::object(fields)
}

fn streamed(at: usize, block: &Value, out: &mut Vec<Value>) {
    let index = || ("index", Value::number(at as u64));
    let kind = block.get("type").and_then(Value::as_str);
    let mut start = block.clone();
    let delta = match (kind, block.get("text"), block.get("input")) {
        (Some("text"), Some(Value::String(text)), _) if !text.is_empty() => {
            start.insert("text", Value::text(""));
            Some(Value::object([
                ("type", Value::text("text_delta")),
                ("text", Value::text(text)),
            ]))
        }
        (Some("tool_use"), _, Some(input)) => {
            start.insert("input", Value::Object(Vec::new()));
            Some(Value::object([
                ("type", Value::text("input_json_delta")),
                ("partial_json", Value::String(input.encode())),
            ]))
        }
        _ => None,
    };
    out.push(event(
        "content_block_start",
        vec![index(), ("content_block", start)],
    ));
    if let Some(delta) = delta {
        out.push(event(
            "content_block_delta",
            vec![index(), ("delta", delta)],
        ));
    }
    out.push(event("content_block_stop", vec![index()]));
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
    let error = |kind: &str, message: String| {
        Value::object([
            ("type", Value::text("error")),
            (
                "error",
                Value::object([
                    ("type", Value::text(kind)),
                    ("message", Value::String(message)),
                ]),
            ),
        ])
        .encode()
    };
    match name {
        "ok" => {
            let stop = fields.get(1).and_then(Json::as_str).unwrap_or("");
            let reason = STOPS
                .iter()
                .find(|(drawn, _)| *drawn == stop)
                .map_or("end_turn", |(_, wired)| *wired);
            let head = Value::object([
                ("id", Value::text("msg_sim")),
                ("type", Value::text("message")),
                ("role", Value::text("assistant")),
                ("content", Value::Array(Vec::new())),
                (
                    "usage",
                    Value::object([
                        ("input_tokens", Value::number(number(2))),
                        ("cache_read_input_tokens", Value::number(number(4))),
                        ("cache_creation_input_tokens", Value::number(number(5))),
                        ("output_tokens", Value::number(0)),
                    ]),
                ),
            ]);
            let mut out = vec![event("message_start", vec![("message", head)])];
            let reply = fields.first().cloned().unwrap_or(Json::Null);
            for (at, block) in blocks(&reply).iter().enumerate() {
                streamed(at, block, &mut out);
            }
            out.push(event(
                "message_delta",
                vec![
                    (
                        "delta",
                        Value::object([("stop_reason", Value::text(reason))]),
                    ),
                    (
                        "usage",
                        Value::object([("output_tokens", Value::number(number(3)))]),
                    ),
                ],
            ));
            out.push(event("message_stop", Vec::new()));
            wire::Answer::Http(wire::Http {
                status: HTTP_OK,
                headers: vec![("content-type".to_owned(), "text/event-stream".to_owned())],
                payload: wire::Payload::Events(out.iter().map(Value::encode).collect()),
            })
        }
        "throttled" => wire::Answer::Http(wire::Http {
            status: HTTP_TOO_MANY,
            headers: wire::retry_after(number(1)),
            payload: wire::Payload::Body(error("rate_limit_error", text(0))),
        }),
        "transient" if number(1) == 0 => wire::Answer::Transport(text(0)),
        "transient" => wire::Answer::Http(wire::Http {
            status: HTTP_SERVER_ERROR,
            headers: wire::retry_after(number(1)),
            payload: wire::Payload::Body(error("api_error", text(0))),
        }),
        "rejected" => wire::Answer::Http(wire::Http {
            status: HTTP_BAD_REQUEST,
            headers: Vec::new(),
            payload: wire::Payload::Body(error("invalid_request_error", text(0))),
        }),
        _ => wire::Answer::Stopped,
    }
}
