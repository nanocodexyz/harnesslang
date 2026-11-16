use std::fmt::Write as _;

use crate::run::json::Json;

pub const DEPTH_MAX: u32 = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn object<'k>(entries: impl IntoIterator<Item = (&'k str, Value)>) -> Self {
        Self::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    pub fn text(text: &str) -> Self {
        Self::String(text.to_owned())
    }

    pub fn number(number: u64) -> Self {
        Self::Number(number.to_string())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(text) if text.bytes().all(|byte| byte.is_ascii_digit()) => {
                text.parse().ok()
            }
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn insert(&mut self, key: &str, value: Value) {
        let Self::Object(entries) = self else {
            panic!("insert into a JSON object only");
        };
        match entries.iter_mut().find(|(name, _)| name == key) {
            Some(entry) => entry.1 = value,
            None => entries.push((key.to_owned(), value)),
        }
    }

    pub fn encode(&self) -> String {
        let mut out = String::new();
        self.encode_into(&mut out);
        out
    }

    pub fn encode_into(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Self::Number(text) => out.push_str(text),
            Self::String(text) => encode_string(text, out),
            Self::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.encode_into(out);
                }
                out.push(']');
            }
            Self::Object(entries) => {
                out.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    encode_string(key, out);
                    out.push(':');
                    value.encode_into(out);
                }
                out.push('}');
            }
        }
    }

    pub fn decode(text: &str) -> Option<Value> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            at: 0,
        };
        let value = parser.value(0)?;
        parser.space();
        (parser.at == parser.bytes.len()).then_some(value)
    }

    pub fn of_runtime(json: &Json) -> Value {
        match json {
            Json::Null => Self::Null,
            Json::Bool(value) => Self::Bool(*value),
            Json::Number(_) | Json::Fraction(_) => Self::Number(json.encode()),
            Json::String(text) => Self::text(text),
            Json::Array(items) => Self::Array(items.iter().map(Self::of_runtime).collect()),
            Json::Object(map) => Self::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), Self::of_runtime(value)))
                    .collect(),
            ),
        }
    }
}

pub fn encode_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if u32::from(control) < 0x20 => {
                write!(out, "\\u{:04x}", u32::from(control)).expect("writing to a string succeeds");
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn word(&mut self, word: &str) -> Option<()> {
        self.bytes[self.at..].starts_with(word.as_bytes()).then(|| {
            self.at += word.len();
        })
    }

    fn value(&mut self, depth: u32) -> Option<Value> {
        if depth >= DEPTH_MAX {
            return None;
        }
        self.space();
        match self.peek()? {
            b'n' => self.word("null").map(|()| Value::Null),
            b't' => self.word("true").map(|()| Value::Bool(true)),
            b'f' => self.word("false").map(|()| Value::Bool(false)),
            b'"' => self.string().map(Value::String),
            b'[' => self.array(depth),
            b'{' => self.object(depth),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn digits(&mut self) -> Option<()> {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        (self.at > start).then_some(())
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        if self.peek() == Some(b'0') {
            self.at += 1;
        } else {
            self.digits()?;
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            self.digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            self.digits()?;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at]).ok()?;
        Some(Value::Number(text.to_owned()))
    }

    fn array(&mut self, depth: u32) -> Option<Value> {
        self.at += 1;
        let mut items = Vec::new();
        self.space();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Some(Value::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.space();
            match self.peek()? {
                b',' => self.at += 1,
                b']' => {
                    self.at += 1;
                    return Some(Value::Array(items));
                }
                _ => return None,
            }
        }
    }

    fn object(&mut self, depth: u32) -> Option<Value> {
        self.at += 1;
        let mut entries: Vec<(String, Value)> = Vec::new();
        self.space();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Some(Value::Object(entries));
        }
        loop {
            self.space();
            if self.peek()? != b'"' {
                return None;
            }
            let key = self.string()?;
            if entries.iter().any(|(name, _)| *name == key) {
                return None;
            }
            self.space();
            self.word(":")?;
            let value = self.value(depth + 1)?;
            entries.push((key, value));
            self.space();
            match self.peek()? {
                b',' => self.at += 1,
                b'}' => {
                    self.at += 1;
                    return Some(Value::Object(entries));
                }
                _ => return None,
            }
        }
    }

    fn hex(&mut self) -> Option<u32> {
        let digits = self.bytes.get(self.at..self.at + 4)?;
        let mut code = 0;
        for digit in digits {
            code = code * 16 + char::from(*digit).to_digit(16)?;
        }
        self.at += 4;
        Some(code)
    }

    fn string(&mut self) -> Option<String> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let start = self.at;
            while !matches!(self.peek()?, b'"' | b'\\') {
                if self.peek()? < 0x20 {
                    return None;
                }
                self.at += 1;
            }
            out.push_str(std::str::from_utf8(&self.bytes[start..self.at]).ok()?);
            if self.peek()? == b'"' {
                self.at += 1;
                return Some(out);
            }
            self.at += 1;
            let escape = self.peek()?;
            self.at += 1;
            match escape {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    let first = self.hex()?;
                    let code = if (0xd800..0xdc00).contains(&first) {
                        self.word("\\u")?;
                        let second = self.hex()?;
                        if !(0xdc00..0xe000).contains(&second) {
                            return None;
                        }
                        0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
                    } else {
                        first
                    };
                    out.push(char::from_u32(code)?);
                }
                _ => return None,
            }
        }
    }
}
