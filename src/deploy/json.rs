use std::collections::BTreeMap;
use std::fmt::Write;

pub const API_JSON_DEPTH_MAX: u32 = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    pub fn object<const N: usize>(entries: [(&str, Value); N]) -> Self {
        let map: BTreeMap<String, Value> = entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        assert_eq!(map.len(), N, "an object's keys are distinct");
        Self::Object(map)
    }

    pub fn text(value: &str) -> Self {
        Self::String(value.to_owned())
    }

    pub fn number(value: u64) -> Self {
        Self::Number(value.to_string())
    }

    pub fn texts(values: &[&str]) -> Self {
        Self::Array(values.iter().map(|value| Self::text(value)).collect())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Object(map) => map.get(key),
            _ => None,
        }
    }

    pub fn path(&self, keys: &[&str]) -> Option<&Value> {
        keys.iter().try_fold(self, |value, key| value.get(key))
    }

    pub fn str_at(&self, keys: &[&str]) -> Option<&str> {
        self.path(keys).and_then(Value::as_str)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn items(&self) -> &[Value] {
        match self {
            Self::Array(items) => items,
            _ => &[],
        }
    }

    pub fn shown(&self) -> String {
        match self {
            Self::String(text) | Self::Number(text) => text.clone(),
            Self::Null => String::new(),
            other => other.encode(),
        }
    }

    pub fn encode(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, depth: u32) {
        assert!(
            depth <= API_JSON_DEPTH_MAX,
            "values we encode are nested at most api_json_depth_max deep"
        );
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Self::Number(text) => out.push_str(text),
            Self::String(value) => write_string(out, value),
            Self::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out, depth + 1);
                }
                out.push(']');
            }
            Self::Object(map) => {
                out.push('{');
                for (index, (key, value)) in map.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(out, key);
                    out.push(':');
                    value.write(out, depth + 1);
                }
                out.push('}');
            }
        }
    }
}

fn write_string(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
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

pub fn decode(text: &str) -> Result<Value, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        offset: 0,
    };
    parser.whitespace();
    let value = parser.value(0)?;
    parser.whitespace();
    if parser.offset != parser.bytes.len() {
        return Err(format!(
            "unexpected text after the JSON value at byte {}",
            parser.offset
        ));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Parser<'_> {
    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.offset += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.offset).copied()
    }

    fn error(&self, what: &str) -> String {
        format!("{what} at byte {}", self.offset)
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        if self.bytes[self.offset..].starts_with(word.as_bytes()) {
            self.offset += word.len();
            Ok(value)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn value(&mut self, depth: u32) -> Result<Value, String> {
        if depth > API_JSON_DEPTH_MAX {
            return Err(self.error("JSON nested deeper than api_json_depth_max (128)"));
        }
        match self.peek() {
            Some(b'n') => self.literal("null", Value::Null),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'"') => self.string().map(Value::String),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(b'-' | b'0'..=b'9') => Ok(self.number()),
            _ => Err(self.error("expected a JSON value")),
        }
    }

    fn number(&mut self) -> Value {
        let start = self.offset;
        while matches!(
            self.peek(),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.offset += 1;
        }
        let text =
            std::str::from_utf8(&self.bytes[start..self.offset]).expect("number bytes are ASCII");
        Value::Number(text.to_owned())
    }

    fn array(&mut self, depth: u32) -> Result<Value, String> {
        self.offset += 1;
        let mut items = Vec::new();
        self.whitespace();
        if self.peek() == Some(b']') {
            self.offset += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.whitespace();
            items.push(self.value(depth + 1)?);
            self.whitespace();
            match self.peek() {
                Some(b',') => self.offset += 1,
                Some(b']') => {
                    self.offset += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self, depth: u32) -> Result<Value, String> {
        self.offset += 1;
        let mut map = BTreeMap::new();
        self.whitespace();
        if self.peek() == Some(b'}') {
            self.offset += 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected an object key"));
            }
            let key = self.string()?;
            self.whitespace();
            if self.peek() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.offset += 1;
            self.whitespace();
            map.insert(key, self.value(depth + 1)?);
            self.whitespace();
            match self.peek() {
                Some(b',') => self.offset += 1,
                Some(b'}') => {
                    self.offset += 1;
                    return Ok(Value::Object(map));
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.offset += 1;
        let mut out = String::new();
        loop {
            let start = self.offset;
            while !matches!(self.peek(), Some(b'"' | b'\\') | None) {
                self.offset += 1;
            }
            out.push_str(
                std::str::from_utf8(&self.bytes[start..self.offset])
                    .map_err(|_| self.error("invalid UTF-8 in a string"))?,
            );
            match self.peek() {
                Some(b'"') => {
                    self.offset += 1;
                    return Ok(out);
                }
                Some(b'\\') => self.escape(&mut out)?,
                _ => return Err(self.error("unterminated string")),
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), String> {
        self.offset += 1;
        let Some(byte) = self.peek() else {
            return Err(self.error("unterminated escape"));
        };
        self.offset += 1;
        match byte {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let code = self.unicode()?;
                out.push(char::from_u32(code).ok_or_else(|| self.error("invalid escape"))?);
            }
            _ => return Err(self.error("invalid escape")),
        }
        Ok(())
    }

    fn unicode(&mut self) -> Result<u32, String> {
        let high = self.hex4()?;
        if !(0xd800..0xdc00).contains(&high) {
            return Ok(high);
        }
        if !self.bytes[self.offset..].starts_with(b"\\u") {
            return Err(self.error("lone surrogate"));
        }
        self.offset += 2;
        let low = self.hex4()?;
        if !(0xdc00..0xe000).contains(&low) {
            return Err(self.error("invalid low surrogate"));
        }
        Ok(0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .bytes
            .get(self.offset..self.offset + 4)
            .ok_or_else(|| self.error("short unicode escape"))?;
        let text = std::str::from_utf8(digits).map_err(|_| self.error("invalid escape"))?;
        let code = u32::from_str_radix(text, 16).map_err(|_| self.error("invalid escape"))?;
        self.offset += 4;
        Ok(code)
    }
}
