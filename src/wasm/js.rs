use crate::run::json::JSON_DEPTH_MAX;

const TEXT_INDEX_LIMIT: u64 = u32::MAX as u64;
const NUMBER_EXPONENT_LIMIT: i32 = 21;
const NUMBER_FRACTION_LIMIT: i32 = -6;
const SURROGATE_HIGH: std::ops::RangeInclusive<u32> = 0xd800..=0xdbff;
const SURROGATE_LOW: std::ops::RangeInclusive<u32> = 0xdc00..=0xdfff;
const SURROGATE_BASE: u32 = 0x10000;
const SURROGATE_SHIFT: u32 = 10;
const CONTROL_LIMIT: u32 = 0x20;
const HEX_DIGITS: usize = 4;

pub static UNDEFINED: Json = Json::Undefined;

#[derive(Clone, Debug)]
pub enum Json {
    Undefined,
    Null,
    Bool(bool),
    Int(String),
    Number(f64),
    Text(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn text(value: &str) -> Json {
        Json::Text(value.to_owned())
    }

    pub fn int(value: impl std::fmt::Display) -> Json {
        Json::Int(value.to_string())
    }

    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map_or(&UNDEFINED, |(_, value)| value),
            _ => &UNDEFINED,
        }
    }

    pub fn or_null(&self) -> &Json {
        match self {
            Json::Undefined | Json::Null => &Json::Null,
            other => other,
        }
    }

    pub fn is_text(&self, text: &str) -> bool {
        matches!(self, Json::Text(value) if value == text)
    }

    pub fn keys(&self) -> Vec<&str> {
        let Json::Object(entries) = self else {
            return Vec::new();
        };
        let mut indexes: Vec<(u64, &str)> = entries
            .iter()
            .filter_map(|(key, _)| array_index(key).map(|index| (index, key.as_str())))
            .collect();
        indexes.sort_unstable();
        let rest = entries
            .iter()
            .filter(|(key, _)| array_index(key).is_none())
            .map(|(key, _)| key.as_str());
        indexes
            .into_iter()
            .map(|(_, key)| key)
            .chain(rest)
            .collect()
    }

    pub fn encode(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Undefined => out.push_str("undefined"),
            Json::Null => out.push_str("null"),
            Json::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Json::Int(digits) => out.push_str(digits),
            Json::Number(value) if value.is_finite() => out.push_str(&number_text(*value)),
            Json::Number(_) => out.push_str("null"),
            Json::Text(value) => write_string(out, value),
            Json::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(entries) => {
                let mut sorted: Vec<&(String, Json)> = entries.iter().collect();
                sorted.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
                out.push('{');
                for (index, (key, value)) in sorted.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(out, key);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn concat(&self) -> String {
        match self {
            Json::Undefined => "undefined".to_owned(),
            Json::Null => "null".to_owned(),
            Json::Bool(value) => value.to_string(),
            Json::Int(digits) => digits.clone(),
            Json::Number(value) if value.is_nan() => "NaN".to_owned(),
            Json::Number(value) if value.is_infinite() => if *value > 0.0 {
                "Infinity"
            } else {
                "-Infinity"
            }
            .to_owned(),
            Json::Number(value) => number_text(*value),
            Json::Text(value) => value.clone(),
            Json::Array(items) => items
                .iter()
                .map(|item| match item {
                    Json::Undefined | Json::Null => String::new(),
                    other => other.concat(),
                })
                .collect::<Vec<_>>()
                .join(","),
            Json::Object(_) => "[object Object]".to_owned(),
        }
    }

    pub fn bigint(&self) -> Result<i64, String> {
        let digits = match self {
            Json::Int(digits) => digits.as_str(),
            Json::Bool(value) => return Ok(i64::from(*value)),
            Json::Text(text) => text.trim(),
            Json::Number(value) if value.fract() == 0.0 && value.is_finite() => {
                return Ok(*value as i128 as i64);
            }
            Json::Number(value) => {
                return Err(format!(
                    "RangeError: The number {} cannot be converted to a BigInt because it is not an integer",
                    Json::Number(*value).concat()
                ));
            }
            other => {
                return Err(format!(
                    "TypeError: Cannot convert {} to a BigInt",
                    other.concat()
                ));
            }
        };
        wrapped(digits).ok_or_else(|| format!("SyntaxError: Cannot convert {digits} to a BigInt"))
    }
}

fn array_index(key: &str) -> Option<u64> {
    let index: u64 = key.parse().ok()?;
    (index < TEXT_INDEX_LIMIT && index.to_string() == key).then_some(index)
}

fn wrapped(digits: &str) -> Option<i64> {
    let (negative, body) = match digits.strip_prefix('-') {
        Some(body) => (true, body),
        None => (false, digits.strip_prefix('+').unwrap_or(digits)),
    };
    if body.is_empty() || !body.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut value: u64 = 0;
    for byte in body.bytes() {
        value = value.wrapping_mul(10).wrapping_add(u64::from(byte - b'0'));
    }
    Some(if negative {
        value.wrapping_neg() as i64
    } else {
        value as i64
    })
}

pub fn number_text(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("scientific notation has an exponent");
    let digits: String = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    let exponent: i32 = exponent.parse().expect("an exponent is an integer");
    let count = digits.len() as i32;
    let point = exponent + 1;
    let body = if count <= point && point <= NUMBER_EXPONENT_LIMIT {
        format!("{digits}{}", "0".repeat((point - count) as usize))
    } else if 0 < point && point <= NUMBER_EXPONENT_LIMIT {
        format!(
            "{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    } else if NUMBER_FRACTION_LIMIT < point && point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let rest = if count > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        let shown = point - 1;
        format!(
            "{}{rest}e{}{}",
            &digits[..1],
            if shown < 0 { '-' } else { '+' },
            shown.abs()
        )
    };
    format!("{sign}{body}")
}

fn write_string(out: &mut String, value: &str) {
    out.reserve(value.len() + 2);
    out.push('"');
    let bytes = value.as_bytes();
    let mut plain = 0;
    for (at, byte) in bytes.iter().enumerate() {
        let escape = match *byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            0x08 => "\\b",
            0x0c => "\\f",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            control if u32::from(control) < CONTROL_LIMIT => "",
            _ => continue,
        };
        out.push_str(&value[plain..at]);
        if escape.is_empty() {
            out.push_str(&format!("\\u{:04x}", byte));
        } else {
            out.push_str(escape);
        }
        plain = at + 1;
    }
    out.push_str(&value[plain..]);
    out.push('"');
}

pub fn short(text: &str, characters_max: usize) -> String {
    if text.encode_utf16().count() <= characters_max {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut units = 0;
    for character in text.chars() {
        let width = character.len_utf16();
        if units + width > characters_max {
            if units < characters_max {
                out.push(char::REPLACEMENT_CHARACTER);
            }
            break;
        }
        units += width;
        out.push(character);
    }
    out.push_str("...");
    out
}

pub fn parse(text: &str) -> Result<Json, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        at: 0,
    };
    parser.space();
    let value = parser.value(0)?;
    parser.space();
    if parser.at != parser.bytes.len() {
        return parser.fail("Unexpected non-whitespace character after JSON");
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn fail<T>(&self, message: &str) -> Result<T, String> {
        Err(format!(
            "SyntaxError: {message} at position {} in JSON",
            self.at
        ))
    }

    fn space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.text[self.at..].starts_with(word) {
            self.at += word.len();
            return Ok(value);
        }
        self.fail("Unexpected token")
    }

    fn value(&mut self, depth: u32) -> Result<Json, String> {
        if depth > JSON_DEPTH_MAX {
            return self.fail("JSON nests deeper than json_depth_max");
        }
        match self.bytes.get(self.at) {
            Some(b'n') => self.literal("null", Json::Null),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::Text),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => self.fail("Unexpected token"),
            None => self.fail("Unexpected end of JSON input"),
        }
    }

    fn array(&mut self, depth: u32) -> Result<Json, String> {
        self.at += 1;
        let mut items = Vec::new();
        self.space();
        if self.bytes.get(self.at) == Some(&b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.space();
            items.push(self.value(depth + 1)?);
            self.space();
            match self.bytes.get(self.at) {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                _ => return self.fail("Expected ',' or ']' after array element"),
            }
        }
    }

    fn object(&mut self, depth: u32) -> Result<Json, String> {
        self.at += 1;
        let mut entries: Vec<(String, Json)> = Vec::new();
        self.space();
        if self.bytes.get(self.at) == Some(&b'}') {
            self.at += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.space();
            if self.bytes.get(self.at) != Some(&b'"') {
                return self.fail("Expected double-quoted property name");
            }
            let key = self.string()?;
            self.space();
            if self.bytes.get(self.at) != Some(&b':') {
                return self.fail("Expected ':' after property name");
            }
            self.at += 1;
            self.space();
            let value = self.value(depth + 1)?;
            match entries.iter_mut().find(|(name, _)| *name == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key, value)),
            }
            self.space();
            match self.bytes.get(self.at) {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(entries));
                }
                _ => return self.fail("Expected ',' or '}' after property value"),
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        if self.bytes[self.at] == b'-' {
            self.at += 1;
        }
        let leading = self.at;
        if self.digits() == 0 {
            return self.fail("No number after minus sign");
        }
        if self.bytes[leading] == b'0' && self.at - leading > 1 {
            self.at = leading + 1;
            return self.fail("Unexpected number");
        }
        let mut integer = true;
        if self.bytes.get(self.at) == Some(&b'.') {
            self.at += 1;
            integer = false;
            if self.digits() == 0 {
                return self.fail("Unterminated fractional number");
            }
        }
        if matches!(self.bytes.get(self.at), Some(b'e' | b'E')) {
            self.at += 1;
            integer = false;
            if matches!(self.bytes.get(self.at), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return self.fail("Exponent part is missing a number");
            }
        }
        let source = &self.text[start..self.at];
        if integer {
            let normalized = if source == "-0" { "0" } else { source };
            return Ok(Json::Int(normalized.to_owned()));
        }
        Ok(Json::Number(
            source.parse().expect("a JSON number parses as f64"),
        ))
    }

    fn hex(&mut self) -> Result<u32, String> {
        let Some(digits) = self.text.get(self.at..self.at + HEX_DIGITS) else {
            return self.fail("Bad Unicode escape");
        };
        let Ok(value) = u32::from_str_radix(digits, 16) else {
            return self.fail("Bad Unicode escape");
        };
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return self.fail("Bad Unicode escape");
        }
        self.at += HEX_DIGITS;
        Ok(value)
    }

    fn escape(&mut self, out: &mut String) -> Result<(), String> {
        let Some(&byte) = self.bytes.get(self.at) else {
            return self.fail("Unterminated string");
        };
        self.at += 1;
        let character = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let unit = self.hex()?;
                let code =
                    if SURROGATE_HIGH.contains(&unit) && self.text[self.at..].starts_with("\\u") {
                        self.at += 2;
                        let low = self.hex()?;
                        if !SURROGATE_LOW.contains(&low) {
                            return self.fail("a lone surrogate escape is not representable");
                        }
                        SURROGATE_BASE
                            + ((unit - SURROGATE_HIGH.start()) << SURROGATE_SHIFT)
                            + (low - SURROGATE_LOW.start())
                    } else {
                        unit
                    };
                match char::from_u32(code) {
                    Some(character) => character,
                    None => return self.fail("a lone surrogate escape is not representable"),
                }
            }
            _ => return self.fail("Bad escaped character"),
        };
        out.push(character);
        Ok(())
    }

    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let start = self.at;
            while self.bytes.get(self.at).is_some_and(|byte| {
                *byte != b'"' && *byte != b'\\' && u32::from(*byte) >= CONTROL_LIMIT
            }) {
                self.at += 1;
            }
            out.push_str(&self.text[start..self.at]);
            match self.bytes.get(self.at) {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    self.escape(&mut out)?;
                }
                Some(_) => return self.fail("Bad control character in string literal"),
                None => return self.fail("Unterminated string in JSON"),
            }
        }
    }
}
