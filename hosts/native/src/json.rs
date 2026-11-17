use std::fmt::Write as _;

pub const DEPTH_MAX: usize = 128;

#[derive(Clone, Debug, PartialEq)]
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
            Self::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    pub fn encode(&self) -> String {
        let mut out = String::new();
        self.encode_into(&mut out);
        out
    }

    fn encode_into(&self, out: &mut String) {
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

    pub fn decode(text: &str) -> Result<Value, String> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            at: 0,
        };
        let value = parser.value(0)?;
        parser.space();
        if parser.at != parser.bytes.len() {
            return Err(format!("trailing bytes at {}", parser.at));
        }
        Ok(value)
    }
}

fn encode_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", control as u32);
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
    fn space(&mut self) {
        while self.at < self.bytes.len()
            && matches!(self.bytes[self.at], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.at += 1;
        }
    }

    fn expect(&mut self, word: &str) -> Result<(), String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(())
        } else {
            Err(format!("expected {word} at byte {}", self.at))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth >= DEPTH_MAX {
            return Err(format!("JSON depth > DEPTH_MAX ({DEPTH_MAX})"));
        }
        self.space();
        let Some(&byte) = self.bytes.get(self.at) else {
            return Err("truncated JSON".to_owned());
        };
        match byte {
            b'n' => self.expect("null").map(|()| Value::Null),
            b't' => self.expect("true").map(|()| Value::Bool(true)),
            b'f' => self.expect("false").map(|()| Value::Bool(false)),
            b'"' => self.string().map(Value::String),
            b'[' => self.array(depth),
            b'{' => self.object(depth),
            b'-' | b'0'..=b'9' => self.number(),
            other => Err(format!("unexpected byte {other:#04x} at byte {}", self.at)),
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.at;
        while self.at < self.bytes.len()
            && matches!(
                self.bytes[self.at],
                b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
            )
        {
            self.at += 1;
        }
        let text =
            std::str::from_utf8(&self.bytes[start..self.at]).map_err(|error| error.to_string())?;
        if text.parse::<f64>().is_err() {
            return Err(format!("invalid number {text} at byte {start}"));
        }
        Ok(Value::Number(text.to_owned()))
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.at += 1;
        let mut items = Vec::new();
        self.space();
        if self.bytes.get(self.at) == Some(&b']') {
            self.at += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.space();
            match self.bytes.get(self.at) {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(format!("expected , or ] at byte {}", self.at)),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.at += 1;
        let mut entries: Vec<(String, Value)> = Vec::new();
        self.space();
        if self.bytes.get(self.at) == Some(&b'}') {
            self.at += 1;
            return Ok(Value::Object(entries));
        }
        loop {
            self.space();
            if self.bytes.get(self.at) != Some(&b'"') {
                return Err(format!("key expected at {}", self.at));
            }
            let key = self.string()?;
            if entries.iter().any(|(name, _)| *name == key) {
                return Err(format!("duplicate key {key}"));
            }
            self.space();
            self.expect(":")?;
            let value = self.value(depth + 1)?;
            entries.push((key, value));
            self.space();
            match self.bytes.get(self.at) {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Value::Object(entries));
                }
                _ => return Err(format!("expected , or }} at byte {}", self.at)),
            }
        }
    }

    fn hex(&mut self) -> Result<u32, String> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .ok_or("short \\u escape")?;
        let text = std::str::from_utf8(digits).map_err(|error| error.to_string())?;
        self.at += 4;
        u32::from_str_radix(text, 16).map_err(|_| format!("invalid \\u escape {text}"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let start = self.at;
            while self.at < self.bytes.len() && !matches!(self.bytes[self.at], b'"' | b'\\') {
                if self.bytes[self.at] < 0x20 {
                    return Err(format!("control byte at {}", self.at));
                }
                self.at += 1;
            }
            out.push_str(
                std::str::from_utf8(&self.bytes[start..self.at])
                    .map_err(|error| error.to_string())?,
            );
            match self.bytes.get(self.at) {
                None => return Err("unterminated string".to_owned()),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(_) => {
                    self.at += 1;
                    let escape = *self.bytes.get(self.at).ok_or("unterminated escape")?;
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
                                self.expect("\\u")?;
                                let second = self.hex()?;
                                if !(0xdc00..0xe000).contains(&second) {
                                    return Err("unpaired surrogate".to_owned());
                                }
                                0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
                            } else {
                                first
                            };
                            out.push(char::from_u32(code).ok_or("invalid code point")?);
                        }
                        other => return Err(format!("invalid escape \\{}", other as char)),
                    }
                }
            }
        }
    }
}
