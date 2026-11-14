use std::collections::BTreeMap;
use std::fmt::Write;

pub const JSON_DEPTH_MAX: u32 = 128;
pub const FRACTION_BITS: u32 = 32;
pub const FRACTION_DIGITS: usize = 32;
const FRACTION_SCALE: u128 = 5_u128.pow(FRACTION_BITS);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(u64),
    Fraction(u32),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub fn object<const N: usize>(entries: [(&str, Json); N]) -> Self {
        let map: BTreeMap<String, Json> = entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        assert_eq!(map.len(), N, "an object's keys are distinct");
        Self::Object(map)
    }

    pub fn text(value: &str) -> Self {
        Self::String(value.to_owned())
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Object(map) => map.get(key),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn tagged(&self) -> Option<(&str, &Json)> {
        match self {
            Self::Object(map) if map.len() == 1 => {
                let (key, value) = map.iter().next().expect("one entry");
                Some((key.as_str(), value))
            }
            _ => None,
        }
    }

    pub fn encode(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, depth: u32) {
        assert!(
            depth <= JSON_DEPTH_MAX,
            "values we encode are nested at most json_depth_max deep"
        );
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Self::Number(value) => write!(out, "{value}").expect("writing to a string succeeds"),
            Self::Fraction(numerator) => write_fraction(out, *numerator),
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

fn write_fraction(out: &mut String, numerator: u32) {
    let scaled = u128::from(numerator) * FRACTION_SCALE;
    let digits = format!("{scaled:0FRACTION_DIGITS$}");
    assert_eq!(
        digits.len(),
        FRACTION_DIGITS,
        "a fraction below one has fraction_digits decimals"
    );
    let trimmed = digits.trim_end_matches('0');
    let shown = if trimmed.is_empty() { "0" } else { trimmed };
    write!(out, "0.{shown}").expect("writing to a string succeeds");
}

fn fraction_from(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() || digits.len() > FRACTION_DIGITS {
        return None;
    }
    let mut scaled: u128 = 0;
    for index in 0..FRACTION_DIGITS {
        let digit = digits.get(index).map_or(0, |byte| byte - b'0');
        scaled = scaled * 10 + u128::from(digit);
    }
    if scaled % FRACTION_SCALE != 0 {
        return None;
    }
    u32::try_from(scaled / FRACTION_SCALE).ok()
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

pub fn decode(text: &str) -> Result<Json, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        offset: 0,
    };
    parser.whitespace();
    let value = parser.value(0)?;
    parser.whitespace();
    if parser.offset != parser.bytes.len() {
        return Err(format!("trailing bytes at offset {}", parser.offset));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Parser<'_> {
    fn whitespace(&mut self) {
        while matches!(
            self.bytes.get(self.offset),
            Some(b' ' | b'\t' | b'\n' | b'\r')
        ) {
            self.offset += 1;
        }
    }

    fn fail<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at offset {}", self.offset))
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.offset..].starts_with(word.as_bytes()) {
            self.offset += word.len();
            return Ok(value);
        }
        self.fail("invalid literal")
    }

    fn value(&mut self, depth: u32) -> Result<Json, String> {
        if depth > JSON_DEPTH_MAX {
            return self.fail(&format!(
                "nesting deeper than json_depth_max ({JSON_DEPTH_MAX})"
            ));
        }
        match self.bytes.get(self.offset) {
            None => self.fail("unexpected end"),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::String),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(byte) if byte.is_ascii_digit() => self.number(),
            Some(_) => self.fail("unexpected byte"),
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.offset;
        let mut value: u64 = 0;
        while let Some(byte) = self
            .bytes
            .get(self.offset)
            .filter(|byte| byte.is_ascii_digit())
        {
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(byte - b'0')))
                .ok_or_else(|| format!("number does not fit in u64 at offset {start}"))?;
            self.offset += 1;
        }
        if self.offset - start > 1 && self.bytes[start] == b'0' {
            return Err(format!("number has a leading zero at offset {start}"));
        }
        if value == 0 && self.offset - start == 1 && self.bytes.get(self.offset) == Some(&b'.') {
            return self.fraction();
        }
        if matches!(self.bytes.get(self.offset), Some(b'.' | b'e' | b'E' | b'-')) {
            return self.fail("only unsigned integers are numbers here");
        }
        Ok(Json::Number(value))
    }

    fn fraction(&mut self) -> Result<Json, String> {
        self.offset += 1;
        let start = self.offset;
        while self
            .bytes
            .get(self.offset)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            self.offset += 1;
        }
        if matches!(self.bytes.get(self.offset), Some(b'e' | b'E')) {
            return self.fail("a fraction has no exponent here");
        }
        match fraction_from(&self.bytes[start..self.offset]) {
            Some(numerator) => Ok(Json::Fraction(numerator)),
            None => self.fail(&format!(
                "a fraction here is a multiple of 2^-{FRACTION_BITS} below one"
            )),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .bytes
            .get(self.offset..self.offset + 4)
            .ok_or("truncated \\u escape")?;
        let text = std::str::from_utf8(digits).map_err(|_| "invalid \\u escape".to_owned())?;
        let code = u32::from_str_radix(text, 16)
            .map_err(|_| format!("invalid \\u escape at offset {}", self.offset))?;
        self.offset += 4;
        Ok(code)
    }

    fn escape(&mut self, out: &mut String) -> Result<(), String> {
        let Some(&byte) = self.bytes.get(self.offset) else {
            return self.fail("truncated escape");
        };
        self.offset += 1;
        let character = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'u' => {
                let high = self.hex4()?;
                let code = if (0xd800..0xdc00).contains(&high) {
                    if !self.bytes[self.offset..].starts_with(b"\\u") {
                        return self.fail("unpaired surrogate");
                    }
                    self.offset += 2;
                    let low = self.hex4()?;
                    if !(0xdc00..0xe000).contains(&low) {
                        return self.fail("invalid low surrogate");
                    }
                    0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
                } else {
                    high
                };
                char::from_u32(code)
                    .ok_or_else(|| format!("invalid code point at offset {}", self.offset))?
            }
            _ => return self.fail("unknown escape"),
        };
        out.push(character);
        Ok(())
    }

    fn string(&mut self) -> Result<String, String> {
        self.offset += 1;
        let mut out = String::new();
        loop {
            let start = self.offset;
            while let Some(&byte) = self.bytes.get(self.offset) {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.offset += 1;
            }
            let run = std::str::from_utf8(&self.bytes[start..self.offset])
                .map_err(|_| format!("string is not utf-8 at offset {start}"))?;
            out.push_str(run);
            match self.bytes.get(self.offset) {
                Some(b'"') => {
                    self.offset += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.offset += 1;
                    self.escape(&mut out)?;
                }
                Some(_) => return self.fail("control character in string"),
                None => return self.fail("unterminated string"),
            }
        }
    }

    fn array(&mut self, depth: u32) -> Result<Json, String> {
        self.offset += 1;
        let mut items = Vec::new();
        self.whitespace();
        if self.bytes.get(self.offset) == Some(&b']') {
            self.offset += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.whitespace();
            items.push(self.value(depth + 1)?);
            self.whitespace();
            match self.bytes.get(self.offset) {
                Some(b',') => self.offset += 1,
                Some(b']') => {
                    self.offset += 1;
                    return Ok(Json::Array(items));
                }
                _ => return self.fail("expected ',' or ']'"),
            }
        }
    }

    fn object(&mut self, depth: u32) -> Result<Json, String> {
        self.offset += 1;
        let mut map = BTreeMap::new();
        self.whitespace();
        if self.bytes.get(self.offset) == Some(&b'}') {
            self.offset += 1;
            return Ok(Json::Object(map));
        }
        loop {
            self.whitespace();
            if self.bytes.get(self.offset) != Some(&b'"') {
                return self.fail("expected a key");
            }
            let key = self.string()?;
            self.whitespace();
            if self.bytes.get(self.offset) != Some(&b':') {
                return self.fail("expected ':'");
            }
            self.offset += 1;
            self.whitespace();
            let value = self.value(depth + 1)?;
            if map.insert(key.clone(), value).is_some() {
                return Err(format!("duplicate key {key:?}"));
            }
            self.whitespace();
            match self.bytes.get(self.offset) {
                Some(b',') => self.offset += 1,
                Some(b'}') => {
                    self.offset += 1;
                    return Ok(Json::Object(map));
                }
                _ => return self.fail("expected ',' or '}'"),
            }
        }
    }
}
