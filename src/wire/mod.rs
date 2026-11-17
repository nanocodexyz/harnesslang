pub mod anthropic;
pub mod diff;
pub mod json;
pub mod openai;

use crate::ir::Wire;
use crate::run::json::Json;
use json::Value;

pub const UNKNOWN_OUTCOME: &str = "Tool execution interrupted by a host crash; outcome unknown. Do not assume it did not run or automatically repeat it.";
pub const MODE_REFRESH: &str = "refresh";
pub const METHOD: &str = "POST";
pub const STOPPED: &str = "the request was stopped";
pub const MALFORMED: &str = "the host's answer to a wire model call is malformed";
pub const MILLISECONDS_PER_SECOND: u64 = 1_000;
pub const MEDIA_TYPE_DEFAULT: &str = "application/octet-stream";
const HTTP_PAYMENT_REQUIRED: u64 = 402;
const HTTP_SUCCESS_FIRST: u64 = 200;
const HTTP_SUCCESS_LAST: u64 = 299;
const CONTENT_TYPE: &str = "content-type";
const EVENT_STREAM: &str = "text/event-stream";
const EVENT_DEFAULT: &str = "message";
const PAYMENT_SCHEME: &str = "Payment ";
const CHALLENGE_OPEN: &str = "@@hl-challenge@@";
const CHALLENGE_CLOSE: &str = "@@hl-end@@";
const PNG_SIGNATURE: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
const JPEG_SIGNATURE: &[u8] = &[0xff, 0xd8, 0xff];
const GIF_SIGNATURES: [&[u8]; 2] = [b"GIF87a", b"GIF89a"];
const RIFF_SIGNATURE: &[u8] = b"RIFF";
const WEBP_SIGNATURE: &[u8] = b"WEBP";
const WEBP_SIGNATURE_AT: usize = 8;

pub type Attached<'a> = dyn Fn(&str) -> Option<(String, String)> + 'a;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    Events(Vec<String>),
    Body(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Http {
    pub status: u64,
    pub headers: Vec<(String, String)>,
    pub payload: Payload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Http(Http),
    Transport(String),
    Stopped,
}

pub fn pairs(json: &Json) -> Option<Vec<(String, String)>> {
    let Json::Array(items) = json else {
        return None;
    };
    items
        .iter()
        .map(|item| match item {
            Json::Array(pair) if pair.len() == 2 => {
                Some((pair[0].as_str()?.to_owned(), pair[1].as_str()?.to_owned()))
            }
            _ => None,
        })
        .collect()
}

fn pairs_json(pairs: &[(String, String)]) -> Json {
    Json::Array(
        pairs
            .iter()
            .map(|(name, value)| Json::Array(vec![Json::text(name), Json::text(value)]))
            .collect(),
    )
}

pub fn streamed(status: u64, headers: &[(String, String)]) -> bool {
    (HTTP_SUCCESS_FIRST..=HTTP_SUCCESS_LAST).contains(&status)
        && headers.iter().any(|(name, value)| {
            name == CONTENT_TYPE && value.to_ascii_lowercase().contains(EVENT_STREAM)
        })
}

fn sse_flush(found: &mut Vec<(String, String)>, event: &mut String, data: &mut Option<String>) {
    if let Some(data) = data.take() {
        let name = if event.is_empty() {
            EVENT_DEFAULT.to_owned()
        } else {
            event.clone()
        };
        found.push((name, data));
    }
    event.clear();
}

pub fn sse_events(body: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut event = String::new();
    let mut data: Option<String> = None;
    let normalized = body.replace("\r\n", "\n");
    for line in normalized.split('\n') {
        if line.is_empty() {
            sse_flush(&mut found, &mut event, &mut data);
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event = value.to_owned(),
            "data" => match data.as_mut() {
                Some(held) => {
                    held.push('\n');
                    held.push_str(value);
                }
                None => data = Some(value.to_owned()),
            },
            _ => {}
        }
    }
    sse_flush(&mut found, &mut event, &mut data);
    found
}

pub fn sse_body(events: &[(String, String)]) -> String {
    let mut body = String::new();
    for (event, data) in events {
        body.push_str("event: ");
        body.push_str(event);
        body.push('\n');
        for line in data.split('\n') {
            body.push_str("data: ");
            body.push_str(line);
            body.push('\n');
        }
        body.push('\n');
    }
    body
}

impl Answer {
    pub fn of_json(json: &Json) -> Option<Answer> {
        let Json::Object(map) = json else {
            return None;
        };
        if map.len() == 1 {
            if let Some(transport) = map.get("transport") {
                return Some(Answer::Transport(transport.as_str()?.to_owned()));
            }
            if map.get("stopped") == Some(&Json::Bool(true)) {
                return Some(Answer::Stopped);
            }
            return None;
        }
        if map.len() != 3 {
            return None;
        }
        let status = map.get("status")?.as_u64()?;
        let headers = pairs(map.get("headers")?)?;
        let payload = match (map.get("events"), map.get("body")) {
            (Some(events), None) => {
                Payload::Events(pairs(events)?.into_iter().map(|(_, data)| data).collect())
            }
            (None, Some(body)) if streamed(status, &headers) => Payload::Events(
                sse_events(body.as_str()?)
                    .into_iter()
                    .map(|(_, data)| data)
                    .collect(),
            ),
            (None, Some(body)) => Payload::Body(body.as_str()?.to_owned()),
            _ => return None,
        };
        Some(Answer::Http(Http {
            status,
            headers,
            payload,
        }))
    }

    pub fn to_json(&self) -> Json {
        match self {
            Answer::Transport(error) => Json::object([("transport", Json::text(error))]),
            Answer::Stopped => Json::object([("stopped", Json::Bool(true))]),
            Answer::Http(http) => {
                let status = ("status", Json::Number(http.status));
                let headers = ("headers", pairs_json(&http.headers));
                match &http.payload {
                    Payload::Body(body) => {
                        Json::object([status, headers, ("body", Json::text(body))])
                    }
                    Payload::Events(events) => {
                        let events: Vec<(String, String)> = events
                            .iter()
                            .map(|data| {
                                let event = Value::decode(data)
                                    .and_then(|value| {
                                        value.get("type").and_then(Value::as_str).map(str::to_owned)
                                    })
                                    .unwrap_or_default();
                                (event, data.clone())
                            })
                            .collect();
                        Json::object([status, headers, ("events", pairs_json(&events))])
                    }
                }
            }
        }
    }
}

pub fn tagged(value: &Value) -> Option<(&str, Option<&Value>)> {
    match value {
        Value::String(name) => Some((name, None)),
        Value::Object(entries) if entries.len() == 1 => Some((&entries[0].0, Some(&entries[0].1))),
        _ => None,
    }
}

pub fn tagged_json(name: &str, fields: Vec<Json>) -> Json {
    Json::object([(name, Json::Array(fields))])
}

pub fn ok(reply: Json, stop: &str, used: Usage) -> Json {
    tagged_json(
        "ok",
        vec![
            reply,
            Json::text(stop),
            Json::Number(used.input),
            Json::Number(used.output),
            Json::Number(used.cache_read),
            Json::Number(used.cache_write),
        ],
    )
}

pub fn throttled(error: &str, retry_after_ms: u64) -> Json {
    tagged_json(
        "throttled",
        vec![Json::text(error), Json::Number(retry_after_ms)],
    )
}

pub fn transient(error: &str, retry_after_ms: u64) -> Json {
    tagged_json(
        "transient",
        vec![Json::text(error), Json::Number(retry_after_ms)],
    )
}

pub fn rejected(error: &str) -> Json {
    Json::object([("rejected", Json::text(error))])
}

pub fn interrupted() -> Json {
    tagged_json("interrupted", Vec::new())
}

pub fn call(id: &str, tool: &str, input: &str) -> Json {
    Json::object([
        ("id", Json::text(id)),
        ("input", Json::text(input)),
        ("tool", Json::text(tool)),
    ])
}

fn header<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.trim_matches([' ', '\t']))
}

fn whole(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(text.bytes().fold(0_u64, |total, digit| {
        total
            .saturating_mul(10)
            .saturating_add(u64::from(digit - b'0'))
    }))
}

pub fn with_challenge(status: u64, headers: &[(String, String)], text: &str) -> String {
    match header(headers, "www-authenticate") {
        Some(challenge)
            if status == HTTP_PAYMENT_REQUIRED && challenge.starts_with(PAYMENT_SCHEME) =>
        {
            format!("{text} {CHALLENGE_OPEN}{challenge}{CHALLENGE_CLOSE}")
        }
        _ => text.to_owned(),
    }
}

pub fn retry_after_ms(headers: &[(String, String)]) -> u64 {
    if let Some(ms) = header(headers, "retry-after-ms").and_then(whole) {
        return ms;
    }
    header(headers, "retry-after")
        .and_then(whole)
        .map_or(0, |seconds| seconds.saturating_mul(MILLISECONDS_PER_SECOND))
}

pub fn retry_after(ms: u64) -> Vec<(String, String)> {
    if ms == 0 {
        return Vec::new();
    }
    vec![("retry-after-ms".to_owned(), ms.to_string())]
}

pub fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(PNG_SIGNATURE) {
        return Some("image/png");
    }
    if bytes.starts_with(JPEG_SIGNATURE) {
        return Some("image/jpeg");
    }
    if GIF_SIGNATURES
        .iter()
        .any(|signature| bytes.starts_with(signature))
    {
        return Some("image/gif");
    }
    let webp = bytes.starts_with(RIFF_SIGNATURE)
        && bytes.get(WEBP_SIGNATURE_AT..WEBP_SIGNATURE_AT + WEBP_SIGNATURE.len())
            == Some(WEBP_SIGNATURE);
    webp.then_some("image/webp")
}

pub fn render(
    wire: Wire,
    preamble: &str,
    context: &Json,
    refresh: bool,
    attached: &Attached<'_>,
) -> Result<String, String> {
    let context = Value::of_runtime(context);
    let items = match wire {
        Wire::Anthropic => anthropic::render(&context, refresh, attached)?,
        Wire::Openai => openai::render(&context, attached)?,
    };
    let mut out = String::from("[");
    out.push_str(preamble);
    for (index, item) in items.iter().enumerate() {
        if index > 0 || !preamble.is_empty() {
            out.push(',');
        }
        item.encode_into(&mut out);
    }
    out.push(']');
    Ok(out)
}

pub struct Request<'a> {
    pub wire: Wire,
    pub turn: u64,
    pub seq: u64,
    pub mode: Option<&'a str>,
    pub settings: Option<&'a Json>,
    pub context: &'a Json,
}

pub fn request(request: &Request<'_>, attached: &Attached<'_>) -> Result<Json, String> {
    let settings = request
        .settings
        .ok_or("a wire model call carries no settings")?;
    let setting = |name: &str| -> Result<&str, String> {
        settings
            .get(name)
            .and_then(Json::as_str)
            .ok_or_else(|| format!("a wire model call carries no text setting {name}"))
    };
    let preamble = settings
        .get(crate::ir::Wire::PREAMBLE)
        .and_then(Json::as_str)
        .unwrap_or("");
    let headers = Value::decode(setting("headers")?)
        .and_then(|headers| header_pairs(&headers))
        .ok_or("the headers setting is not a JSON list of [name, value] text pairs")?;
    let refresh = request.wire == Wire::Anthropic && request.mode == Some(MODE_REFRESH);
    let rendered = render(request.wire, preamble, request.context, refresh, attached)?;
    let mut body = String::from(setting("head")?);
    body.push_str(&rendered);
    body.push_str(setting("tail")?);
    let http = Json::object([
        ("body", Json::String(body)),
        ("headers", pairs_json(&headers)),
        ("method", Json::text(METHOD)),
        ("url", Json::text(setting("url")?)),
    ]);
    Ok(Json::object([
        ("http", http),
        ("seq", Json::Number(request.seq)),
        ("turn", Json::Number(request.turn)),
    ]))
}

fn header_pairs(headers: &Value) -> Option<Vec<(String, String)>> {
    headers
        .as_array()?
        .iter()
        .map(|pair| match pair.as_array()? {
            [name, value] => Some((name.as_str()?.to_owned(), value.as_str()?.to_owned())),
            _ => None,
        })
        .collect()
}

pub fn decode(wire: Wire, answer: &Json, interruptible: bool) -> Json {
    match Answer::of_json(answer) {
        None => transient(MALFORMED, 0),
        Some(Answer::Transport(error)) => transient(&error, 0),
        Some(Answer::Stopped) if interruptible => interrupted(),
        Some(Answer::Stopped) => transient(STOPPED, 0),
        Some(Answer::Http(http)) => match wire {
            Wire::Anthropic => anthropic::decode(&http),
            Wire::Openai => openai::decode(&http),
        },
    }
}

pub fn encode(wire: Wire, attempt: &Json) -> Json {
    let answer = match wire {
        Wire::Anthropic => anthropic::encode(attempt),
        Wire::Openai => openai::encode(attempt),
    };
    answer.to_json()
}
