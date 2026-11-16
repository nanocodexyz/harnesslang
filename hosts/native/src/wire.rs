use std::path::Path;

use harnesslang::wasm::js::Json;

use crate::transport::{self, Post, Sent};

const EVENT_STREAM: &str = "text/event-stream";
const EVENT_DEFAULT: &str = "message";
const SUCCESS_FIRST: u16 = 200;
const SUCCESS_LAST: u16 = 299;
const PARTIAL_FILE: &str = "wire-partial";
const OPEN: &str = "${";
const DEFAULT_MARK: &str = ":-";

pub fn expand(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("an unclosed {OPEN} in {text:?}"))?;
        let inner = &after[..end];
        let (name, default) = match inner.split_once(DEFAULT_MARK) {
            Some((name, default)) => (name, Some(default)),
            None => (inner, None),
        };
        let valid = !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if !valid {
            return Err(format!("{OPEN}{inner}}} does not name an environment variable"));
        }
        match (lookup(name).filter(|value| !value.is_empty()), default) {
            (Some(value), _) => out.push_str(&value),
            (None, Some(default)) => out.push_str(default),
            (None, None) => return Err(format!("the environment has no {name}")),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn flush(found: &mut Vec<(String, String)>, event: &mut String, data: &mut Option<String>) {
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

pub fn events(body: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut event = String::new();
    let mut data: Option<String> = None;
    let normalized = body.replace("\r\n", "\n");
    for line in normalized.split('\n') {
        if line.is_empty() {
            flush(&mut found, &mut event, &mut data);
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
    flush(&mut found, &mut event, &mut data);
    found
}

fn pairs(pairs: &[(String, String)]) -> Json {
    Json::Array(
        pairs
            .iter()
            .map(|(name, value)| Json::Array(vec![Json::text(name), Json::text(value)]))
            .collect(),
    )
}

fn object(fields: Vec<(&str, Json)>) -> Json {
    Json::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

pub fn answer(response: &transport::Response) -> Json {
    let streamed = response.headers.iter().any(|(name, value)| {
        name == "content-type" && value.to_ascii_lowercase().contains(EVENT_STREAM)
    });
    let success = (SUCCESS_FIRST..=SUCCESS_LAST).contains(&response.status);
    let headers = pairs(&response.headers);
    let status = Json::int(response.status);
    if success && streamed {
        return object(vec![
            ("events", pairs(&events(&response.body))),
            ("headers", headers),
            ("status", status),
        ]);
    }
    object(vec![
        ("body", Json::text(&response.body)),
        ("headers", headers),
        ("status", status),
    ])
}

fn text_of<'a>(json: &'a Json, what: &str) -> Result<&'a str, String> {
    match json {
        Json::Text(text) => Ok(text),
        other => Err(format!("{what} is not text: {}", other.encode())),
    }
}

pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

pub fn request(http: &Json, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Request, String> {
    let method = text_of(http.get("method"), "a wire request's method")?.to_owned();
    let url = expand(text_of(http.get("url"), "a wire request's url")?, lookup)?;
    let body = text_of(http.get("body"), "a wire request's body")?.to_owned();
    let Json::Array(listed) = http.get("headers") else {
        return Err(format!(
            "a wire request's headers are not a list: {}",
            http.get("headers").encode()
        ));
    };
    let mut headers = Vec::with_capacity(listed.len());
    for pair in listed {
        let Json::Array(parts) = pair else {
            return Err(format!("a header is not a [name, value] pair: {}", pair.encode()));
        };
        let [name, value] = parts.as_slice() else {
            return Err(format!("a header is not a [name, value] pair: {}", pair.encode()));
        };
        headers.push((
            expand(text_of(name, "a header's name")?, lookup)?,
            expand(text_of(value, "a header's value")?, lookup)?,
        ));
    }
    Ok(Request {
        method,
        url,
        headers,
        body,
    })
}

pub fn call(http: &Json, scratch: &Path, stop: &dyn Fn() -> bool) -> Json {
    let lookup = |name: &str| std::env::var(name).ok();
    let request = match request(http, &lookup) {
        Ok(request) => request,
        Err(error) => return object(vec![("transport", Json::text(&error))]),
    };
    let partial = scratch.join(PARTIAL_FILE);
    let sent = transport::send(&Post {
        method: &request.method,
        url: &request.url,
        headers: &request.headers,
        body: &request.body,
        scratch,
        partial: &partial,
        stop,
    });
    match sent {
        Ok(Sent::Done(response)) => answer(&response),
        Ok(Sent::Stopped) => object(vec![("stopped", Json::Bool(true))]),
        Err(error) => object(vec![("transport", Json::text(&error))]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(name: &str) -> Option<String> {
        match name {
            "KEY" => Some("secret".to_owned()),
            "EMPTY" => Some(String::new()),
            _ => None,
        }
    }

    #[test]
    fn expands_names_and_defaults() {
        assert_eq!(expand("a ${KEY} b", &lookup).unwrap(), "a secret b");
        assert_eq!(expand("${NONE:-http://x}/v1", &lookup).unwrap(), "http://x/v1");
        assert_eq!(expand("${EMPTY:-d}", &lookup).unwrap(), "d");
        assert_eq!(expand("${KEY:-d}", &lookup).unwrap(), "secret");
        assert!(expand("${NONE}", &lookup).is_err());
        assert!(expand("${KEY", &lookup).is_err());
        assert!(expand("${a b}", &lookup).is_err());
        assert_eq!(expand("no names", &lookup).unwrap(), "no names");
    }

    #[test]
    fn splits_server_sent_events() {
        let body = "event: a\ndata: {\"x\":1}\n\n: comment\ndata: one\ndata: two\n\nevent: b\r\ndata:raw\r\n\r\nevent: tail\ndata: last";
        assert_eq!(
            events(body),
            vec![
                ("a".to_owned(), "{\"x\":1}".to_owned()),
                ("message".to_owned(), "one\ntwo".to_owned()),
                ("b".to_owned(), "raw".to_owned()),
                ("tail".to_owned(), "last".to_owned()),
            ]
        );
        assert!(events("event: ping\n\n").is_empty());
    }

    #[test]
    fn answers_streams_and_bodies() {
        let streamed = transport::Response {
            status: 200,
            headers: vec![(
                "content-type".to_owned(),
                "text/event-stream; charset=utf-8".to_owned(),
            )],
            body: "event: e\ndata: d\n\n".to_owned(),
        };
        assert_eq!(
            answer(&streamed).encode(),
            "{\"events\":[[\"e\",\"d\"]],\"headers\":[[\"content-type\",\"text/event-stream; charset=utf-8\"]],\"status\":200}"
        );
        let refused = transport::Response {
            status: 429,
            headers: vec![("retry-after".to_owned(), "2".to_owned())],
            body: "slow down".to_owned(),
        };
        assert_eq!(
            answer(&refused).encode(),
            "{\"body\":\"slow down\",\"headers\":[[\"retry-after\",\"2\"]],\"status\":429}"
        );
    }
}
