use std::collections::BTreeMap;
use std::fmt::Write as _;

pub const JSON_DEPTH_MAX: u32 = 128;
pub const FIRST_SEQ: u64 = 1;
pub const FIRST_TURN: u64 = 1;
pub const CRC32_POLYNOMIAL_REFLECTED: u32 = 0xedb8_8320;
pub const CRC32_CHECK_INPUT: &[u8] = b"123456789";
pub const CRC32_CHECK: u32 = 0xcbf4_3926;
const BYTE_VALUES: usize = 256;
const BITS_PER_BYTE: u32 = 8;
const LINE_PREFIX: &str = "{\"crc\":";
const LINE_MIDDLE: &str = ",\"record\":";
const LINE_SUFFIX: &str = "}";
pub const LINE_END: u8 = b'\n';

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(u64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub fn object(entries: Vec<(&str, Json)>) -> Self {
        let count = entries.len();
        let map: BTreeMap<String, Json> = entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        assert_eq!(map.len(), count, "an object's keys are distinct");
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
        if matches!(self.bytes.get(self.offset), Some(b'.' | b'e' | b'E' | b'-')) {
            return self.fail("only unsigned integers are numbers here");
        }
        Ok(Json::Number(value))
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

const CRC32_TABLE: [u32; BYTE_VALUES] = crc32_table();

const fn crc32_table() -> [u32; BYTE_VALUES] {
    let mut table = [0_u32; BYTE_VALUES];
    let mut index = 0;
    while index < BYTE_VALUES {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < BITS_PER_BYTE {
            value = if value & 1 == 1 {
                (value >> 1) ^ CRC32_POLYNOMIAL_REFLECTED
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        let index = ((crc ^ u32::from(*byte)) & 0xff) as usize;
        crc = (crc >> BITS_PER_BYTE) ^ CRC32_TABLE[index];
    }
    !crc
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectClass {
    None,
    Idempotent,
    External,
}

impl EffectClass {
    pub const ALL: [Self; 3] = [Self::None, Self::Idempotent, Self::External];

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Idempotent => "idempotent",
            Self::External => "external",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.name() == name)
    }

    pub const fn may_be_unknown(self) -> bool {
        matches!(self, Self::External)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
}

impl Outcome {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Ok(Json),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inbound {
    Cancel,
    Steer(String),
}

impl Inbound {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Steer(_) => "steer",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub id: String,
    pub inbound: Inbound,
}

pub fn delivery_json(delivery: &Delivery) -> Json {
    let mut fields = vec![("id", Json::text(&delivery.id))];
    if let Inbound::Steer(message) = &delivery.inbound {
        fields.push(("message", Json::text(message)));
    }
    Json::object(vec![(delivery.inbound.name(), Json::object(fields))])
}

pub fn delivery_from(json: &Json) -> Result<Delivery, String> {
    let Some((name, Json::Object(map))) = json.tagged() else {
        return Err(format!("event {} is not a tagged object", json.encode()));
    };
    let fields = Fields { tag: name, map };
    let id = fields.text("id")?.to_owned();
    let inbound = match name {
        "cancel" => {
            fields.exact(&["id"])?;
            Inbound::Cancel
        }
        "steer" => {
            fields.exact(&["id", "message"])?;
            Inbound::Steer(fields.text("message")?.to_owned())
        }
        other => return Err(format!("unknown event '{other}'")),
    };
    Ok(Delivery { id, inbound })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Admitted {
        turn: u64,
        handler: String,
        arguments: Json,
    },
    Call {
        turn: u64,
        seq: u64,
        tool: String,
        effect: EffectClass,
        input: Json,
    },
    Result {
        turn: u64,
        seq: u64,
        output: Output,
    },
    Model {
        turn: u64,
        seq: u64,
        response: Json,
    },
    Recovery {
        turn: u64,
        attempt: u64,
        unknown: Vec<u64>,
    },
    Status {
        turn: u64,
        last: u64,
        outcome: Outcome,
        result: Json,
    },
    Checkpoint {
        revision: u64,
        state: String,
        payload: String,
    },
    Event {
        turn: u64,
        seq: u64,
        at: u64,
        delivery: Delivery,
    },
}

impl Record {
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Admitted { .. } => "admitted",
            Self::Call { .. } => "call",
            Self::Result { .. } => "result",
            Self::Model { .. } => "model",
            Self::Recovery { .. } => "recovery",
            Self::Status { .. } => "status",
            Self::Checkpoint { .. } => "checkpoint",
            Self::Event { .. } => "event",
        }
    }

    pub const fn turn(&self) -> u64 {
        match self {
            Self::Admitted { turn, .. }
            | Self::Call { turn, .. }
            | Self::Result { turn, .. }
            | Self::Model { turn, .. }
            | Self::Recovery { turn, .. }
            | Self::Status { turn, .. }
            | Self::Event { turn, .. } => *turn,
            Self::Checkpoint { revision, .. } => *revision,
        }
    }
}

pub fn call_id(turn: u64, seq: u64) -> String {
    format!("{turn}/{seq}")
}

fn parse_call_id(text: &str) -> Result<(u64, u64), String> {
    let (turn, seq) = text
        .split_once('/')
        .ok_or_else(|| format!("call id {text:?} is not turn/seq"))?;
    let turn = turn
        .parse()
        .map_err(|_| format!("call id {text:?} has no turn number"))?;
    let seq = seq
        .parse()
        .map_err(|_| format!("call id {text:?} has no sequence number"))?;
    Ok((turn, seq))
}

fn turn_text(turn: u64) -> Json {
    Json::String(turn.to_string())
}

pub fn output_json(output: &Output) -> Json {
    match output {
        Output::Ok(value) => Json::object(vec![("ok", value.clone())]),
        Output::Failed(error) => Json::object(vec![("failed", Json::text(error))]),
    }
}

pub fn output_from(json: &Json) -> Result<Output, String> {
    match json.tagged() {
        Some(("ok", value)) => Ok(Output::Ok(value.clone())),
        Some(("failed", Json::String(error))) => Ok(Output::Failed(error.clone())),
        _ => Err(format!("output {} is neither ok nor failed", json.encode())),
    }
}

fn record_body(record: &Record) -> Json {
    match record {
        Record::Admitted {
            turn,
            handler,
            arguments,
        } => Json::object(vec![
            ("digest", Json::text(handler)),
            ("first", Json::Number(FIRST_SEQ)),
            ("hand", Json::Null),
            ("metadata", Json::object(vec![("handler", Json::text(handler))])),
            ("prompt", arguments.clone()),
            ("turn", turn_text(*turn)),
        ]),
        Record::Call {
            turn,
            seq,
            tool,
            effect,
            input,
        } => Json::object(vec![
            ("call_id", Json::String(call_id(*turn, *seq))),
            ("effect", Json::text(effect.name())),
            ("input", input.clone()),
            ("seq", Json::Number(*seq)),
            ("tool", Json::text(tool)),
            ("turn", turn_text(*turn)),
        ]),
        Record::Result { turn, seq, output } => Json::object(vec![
            ("call_id", Json::String(call_id(*turn, *seq))),
            ("output", output_json(output)),
            ("seq", Json::Number(*seq)),
        ]),
        Record::Model {
            turn,
            seq,
            response,
        } => Json::object(vec![
            ("call_id", Json::String(call_id(*turn, *seq))),
            ("response", response.clone()),
            ("seq", Json::Number(*seq)),
            ("turn", turn_text(*turn)),
        ]),
        Record::Recovery {
            turn,
            attempt,
            unknown,
        } => Json::object(vec![
            ("attempt", Json::Number(*attempt)),
            ("turn", turn_text(*turn)),
            (
                "unknown",
                Json::Array(
                    unknown
                        .iter()
                        .map(|seq| Json::String(call_id(*turn, *seq)))
                        .collect(),
                ),
            ),
        ]),
        Record::Status {
            turn,
            last,
            outcome,
            result,
        } => {
            let finished = Json::object(vec![
                ("last", Json::Number(*last)),
                ("outcome", Json::text(outcome.name())),
                ("result", result.clone()),
            ]);
            Json::object(vec![
                ("status", Json::object(vec![("finished", finished)])),
                ("turn", turn_text(*turn)),
            ])
        }
        Record::Checkpoint {
            revision,
            state,
            payload,
        } => Json::object(vec![
            ("payload", Json::String(payload.clone())),
            ("revision", Json::Number(*revision)),
            ("state", Json::text(state)),
        ]),
        Record::Event {
            turn,
            seq,
            at,
            delivery,
        } => Json::object(vec![
            ("at", Json::Number(*at)),
            ("event", delivery_json(delivery)),
            ("seq", Json::Number(*seq)),
            ("turn", turn_text(*turn)),
        ]),
    }
}

pub fn record_json(record: &Record) -> Json {
    Json::object(vec![(record.tag(), record_body(record))])
}

struct Fields<'a> {
    tag: &'a str,
    map: &'a BTreeMap<String, Json>,
}

impl<'a> Fields<'a> {
    fn get(&self, key: &str) -> Result<&'a Json, String> {
        self.map
            .get(key)
            .ok_or_else(|| format!("{} record lacks '{key}'", self.tag))
    }

    fn number(&self, key: &str) -> Result<u64, String> {
        self.get(key)?
            .as_u64()
            .ok_or_else(|| format!("{} record's '{key}' is not a number", self.tag))
    }

    fn text(&self, key: &str) -> Result<&'a str, String> {
        self.get(key)?
            .as_str()
            .ok_or_else(|| format!("{} record's '{key}' is not text", self.tag))
    }

    fn turn(&self) -> Result<u64, String> {
        self.text("turn")?
            .parse()
            .map_err(|_| format!("{} record's turn is not a number", self.tag))
    }

    fn call(&self) -> Result<(u64, u64), String> {
        let (turn, seq) = parse_call_id(self.text("call_id")?)?;
        if self.number("seq")? != seq {
            return Err(format!(
                "{} record's seq disagrees with its call id",
                self.tag
            ));
        }
        Ok((turn, seq))
    }

    fn exact(&self, keys: &[&str]) -> Result<(), String> {
        if self.map.len() != keys.len() || keys.iter().any(|key| !self.map.contains_key(*key)) {
            return Err(format!(
                "{} record has fields {:?}, expected {keys:?}",
                self.tag,
                self.map.keys().collect::<Vec<_>>()
            ));
        }
        Ok(())
    }
}

pub fn record_from(json: &Json) -> Result<Record, String> {
    let Some((tag, Json::Object(map))) = json.tagged() else {
        return Err("a record is an object with one tag".to_owned());
    };
    let fields = Fields { tag, map };
    match tag {
        "admitted" => {
            fields.exact(&["digest", "first", "hand", "metadata", "prompt", "turn"])?;
            let handler = fields.text("digest")?.to_owned();
            Ok(Record::Admitted {
                turn: fields.turn()?,
                handler,
                arguments: fields.get("prompt")?.clone(),
            })
        }
        "call" => {
            fields.exact(&["call_id", "effect", "input", "seq", "tool", "turn"])?;
            let (turn, seq) = fields.call()?;
            if fields.turn()? != turn {
                return Err("call record's turn disagrees with its call id".to_owned());
            }
            let effect = EffectClass::parse(fields.text("effect")?)
                .ok_or("call record has an unknown effect class")?;
            Ok(Record::Call {
                turn,
                seq,
                tool: fields.text("tool")?.to_owned(),
                effect,
                input: fields.get("input")?.clone(),
            })
        }
        "result" => {
            fields.exact(&["call_id", "output", "seq"])?;
            let (turn, seq) = fields.call()?;
            Ok(Record::Result {
                turn,
                seq,
                output: output_from(fields.get("output")?)?,
            })
        }
        "model" => {
            fields.exact(&["call_id", "response", "seq", "turn"])?;
            let (turn, seq) = fields.call()?;
            Ok(Record::Model {
                turn,
                seq,
                response: fields.get("response")?.clone(),
            })
        }
        "recovery" => recovery_from(&fields),
        "status" => status_from(&fields),
        "event" => {
            fields.exact(&["at", "event", "seq", "turn"])?;
            Ok(Record::Event {
                turn: fields.turn()?,
                seq: fields.number("seq")?,
                at: fields.number("at")?,
                delivery: delivery_from(fields.get("event")?)?,
            })
        }
        "checkpoint" => {
            fields.exact(&["payload", "revision", "state"])?;
            let payload = fields.text("payload")?.to_owned();
            Ok(Record::Checkpoint {
                revision: fields.number("revision")?,
                state: fields.text("state")?.to_owned(),
                payload,
            })
        }
        other => Err(format!("unknown record '{other}'")),
    }
}

fn recovery_from(fields: &Fields<'_>) -> Result<Record, String> {
    fields.exact(&["attempt", "turn", "unknown"])?;
    let turn = fields.turn()?;
    let Json::Array(items) = fields.get("unknown")? else {
        return Err("recovery record's unknown is not a list".to_owned());
    };
    let mut unknown = Vec::new();
    for item in items {
        let (call_turn, seq) = parse_call_id(
            item.as_str()
                .ok_or("recovery lists a call id that is not text")?,
        )?;
        if call_turn != turn {
            return Err("recovery lists a call of another turn".to_owned());
        }
        unknown.push(seq);
    }
    Ok(Record::Recovery {
        turn,
        attempt: fields.number("attempt")?,
        unknown,
    })
}

fn status_from(fields: &Fields<'_>) -> Result<Record, String> {
    fields.exact(&["status", "turn"])?;
    let Some(("finished", Json::Object(finished))) = fields.get("status")?.tagged() else {
        return Err("status record is not finished".to_owned());
    };
    let inner = Fields {
        tag: "finished",
        map: finished,
    };
    inner.exact(&["last", "outcome", "result"])?;
    let outcome = match inner.text("outcome")? {
        "completed" => Outcome::Completed,
        "failed" => Outcome::Failed,
        other => return Err(format!("unknown outcome '{other}'")),
    };
    Ok(Record::Status {
        turn: fields.turn()?,
        last: inner.number("last")?,
        outcome,
        result: inner.get("result")?.clone(),
    })
}

pub fn encode_line(record: &Record, checksum: bool) -> String {
    let body = record_json(record).encode();
    let line = if checksum {
        format!(
            "{LINE_PREFIX}{}{LINE_MIDDLE}{body}{LINE_SUFFIX}",
            crc32(body.as_bytes())
        )
    } else {
        body
    };
    let decoded = decode_line(&line, checksum).expect("a line we encode decodes");
    assert_eq!(&decoded, record, "a record survives its encoding");
    line
}

pub fn decode_line(line: &str, checksum: bool) -> Result<Record, String> {
    let body = if checksum {
        let rest = line
            .strip_prefix(LINE_PREFIX)
            .ok_or("line does not start with its checksum")?;
        let (crc, rest) = rest
            .split_once(LINE_MIDDLE)
            .ok_or("line has no record after its checksum")?;
        let body = rest
            .strip_suffix(LINE_SUFFIX)
            .ok_or("line does not end its envelope")?;
        let crc: u32 = crc
            .parse()
            .map_err(|_| "line's checksum is not a number".to_owned())?;
        if crc32(body.as_bytes()) != crc {
            return Err("line's checksum does not match its record".to_owned());
        }
        body
    } else {
        line
    };
    record_from(&Json::decode(body)?)
}

pub fn encode_lines(records: &[Record], checksum: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend_from_slice(encode_line(record, checksum).as_bytes());
        bytes.push(LINE_END);
    }
    bytes
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub records: Vec<Record>,
    pub valid_bytes: usize,
    pub dropped_bytes: usize,
}

pub fn decode_checksummed_lines(bytes: &[u8]) -> Decoded {
    let mut records = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let Some(length) = bytes[offset..].iter().position(|byte| *byte == LINE_END) else {
            break;
        };
        let line = &bytes[offset..offset + length];
        let decoded = std::str::from_utf8(line)
            .map_err(|_| "line is not utf-8".to_owned())
            .and_then(|line| decode_line(line, true));
        match decoded {
            Ok(record) => records.push(record),
            Err(_) => break,
        }
        offset += length + 1;
    }
    Decoded {
        records,
        valid_bytes: offset,
        dropped_bytes: bytes.len() - offset,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub tool: String,
    pub effect: EffectClass,
    pub input: Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SiteLog {
    Open(Call),
    Done(Call, Output),
    Unknown(Call),
    Reply(Json),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub outcome: Outcome,
    pub result: Json,
    pub last: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    pub at: u64,
    pub delivery: Delivery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnLog {
    pub handler: String,
    pub arguments: Json,
    pub sites: BTreeMap<u64, SiteLog>,
    pub attempts: u64,
    pub finished: Option<Finished>,
    pub events: Vec<Recorded>,
}

impl TurnLog {
    pub fn recorded(&self, id: &str) -> bool {
        self.events.iter().any(|event| event.delivery.id == id)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Start {
    #[default]
    Beginning,
    Checkpoint(u64),
}

impl Start {
    pub const fn first_turn(self) -> u64 {
        match self {
            Self::Beginning => FIRST_TURN,
            Self::Checkpoint(revision) => revision + 1,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub start: Start,
    pub turns: BTreeMap<u64, TurnLog>,
    pub checkpoints: BTreeMap<u64, String>,
    pub last_event: u64,
}

fn running<'a>(session: &'a mut Session, turn: u64, what: &str) -> Result<&'a mut TurnLog, String> {
    let log = session
        .turns
        .get_mut(&turn)
        .ok_or_else(|| format!("{what} for turn {turn}, which was not admitted"))?;
    if log.finished.is_some() {
        return Err(format!("{what} for turn {turn}, which already finished"));
    }
    Ok(log)
}

fn next_site(log: &mut TurnLog, turn: u64, seq: u64, site: SiteLog) -> Result<(), String> {
    if let Some((last, _)) = log.sites.last_key_value()
        && seq <= *last
    {
        return Err(format!("turn {turn} records seq {seq} after seq {last}"));
    }
    log.sites.insert(seq, site);
    Ok(())
}

pub fn fold(records: &[Record]) -> Result<Session, String> {
    fold_from(Start::Beginning, records)
}

pub fn fold_from(start: Start, records: &[Record]) -> Result<Session, String> {
    let mut session = Session {
        start,
        ..Session::default()
    };
    let rest = match start {
        Start::Beginning => records,
        Start::Checkpoint(revision) => {
            let Some((Record::Checkpoint { revision: first, payload, .. }, rest)) = records.split_first() else {
                return Err(format!("a journal read from checkpoint {revision} does not start with it"));
            };
            if *first != revision {
                return Err(format!("a journal read from checkpoint {revision} starts with checkpoint {first}"));
            }
            session.checkpoints.insert(revision, payload.clone());
            rest
        }
    };
    for record in rest {
        apply(&mut session, record)?;
    }
    Ok(session)
}

fn admit(session: &mut Session, turn: u64, handler: &str, arguments: &Json) -> Result<(), String> {
    if let Start::Checkpoint(revision) = session.start
        && session.turns.is_empty()
        && turn != revision + 1
    {
        return Err(format!("turn {turn} is admitted after checkpoint {revision}"));
    }
    if let Some((last, log)) = session.turns.last_key_value() {
        if turn != last + 1 {
            return Err(format!("turn {turn} is admitted after turn {last}"));
        }
        if log.finished.is_none() {
            return Err(format!(
                "turn {turn} is admitted before turn {last} finished"
            ));
        }
    }
    session.turns.insert(
        turn,
        TurnLog {
            handler: handler.to_owned(),
            arguments: arguments.clone(),
            sites: BTreeMap::new(),
            attempts: 0,
            finished: None,
            events: Vec::new(),
        },
    );
    Ok(())
}

fn event(session: &mut Session, turn: u64, seq: u64, at: u64, delivery: &Delivery) -> Result<(), String> {
    if seq <= session.last_event {
        return Err(format!(
            "event {seq} of turn {turn} follows event {}",
            session.last_event
        ));
    }
    let log = running(session, turn, "an event")?;
    if let Some(last) = log.events.last()
        && at < last.at
    {
        return Err(format!(
            "event {seq} of turn {turn} is at seq {at}, before the event at seq {}",
            last.at
        ));
    }
    if log.recorded(&delivery.id) {
        return Err(format!(
            "event '{}' of turn {turn} is recorded twice",
            delivery.id
        ));
    }
    let cancelled = log
        .events
        .iter()
        .any(|event| event.delivery.inbound == Inbound::Cancel);
    if cancelled && delivery.inbound == Inbound::Cancel {
        return Err(format!("turn {turn} records a second cancel"));
    }
    log.events.push(Recorded {
        at,
        delivery: delivery.clone(),
    });
    session.last_event = seq;
    Ok(())
}

fn close(session: &mut Session, turn: u64, seq: u64, output: &Output) -> Result<(), String> {
    let log = running(session, turn, "a result")?;
    let site = log
        .sites
        .get_mut(&seq)
        .ok_or_else(|| format!("result for seq {seq} of turn {turn}, which has no call"))?;
    let SiteLog::Open(call) = site else {
        return Err(format!(
            "result for seq {seq} of turn {turn}, which is not open"
        ));
    };
    *site = SiteLog::Done(call.clone(), output.clone());
    Ok(())
}

fn recover_record(session: &mut Session, turn: u64, attempt: u64, unknown: &[u64]) -> Result<(), String> {
    let log = running(session, turn, "a recovery")?;
    if attempt <= log.attempts {
        return Err(format!(
            "recovery attempt {attempt} of turn {turn} follows attempt {}",
            log.attempts
        ));
    }
    log.attempts = attempt;
    for seq in unknown {
        let site = log.sites.get_mut(seq).ok_or_else(|| {
            format!("recovery of seq {seq} of turn {turn}, which has no call")
        })?;
        let SiteLog::Open(call) = site else {
            return Err(format!(
                "recovery of seq {seq} of turn {turn}, which is not open"
            ));
        };
        if !call.effect.may_be_unknown() {
            return Err(format!(
                "recovery reports {} call seq {seq} of turn {turn} unknown",
                call.effect.name()
            ));
        }
        *site = SiteLog::Unknown(call.clone());
    }
    Ok(())
}

pub fn apply(session: &mut Session, record: &Record) -> Result<(), String> {
    match record {
        Record::Admitted {
            turn,
            handler,
            arguments,
        } => admit(session, *turn, handler, arguments)?,
        Record::Call {
            turn,
            seq,
            tool,
            effect,
            input,
        } => {
            let log = running(session, *turn, "a call")?;
            let call = Call {
                tool: tool.clone(),
                effect: *effect,
                input: input.clone(),
            };
            next_site(log, *turn, *seq, SiteLog::Open(call))?;
        }
        Record::Result { turn, seq, output } => close(session, *turn, *seq, output)?,
        Record::Model {
            turn,
            seq,
            response,
        } => {
            let log = running(session, *turn, "a model reply")?;
            next_site(log, *turn, *seq, SiteLog::Reply(response.clone()))?;
        }
        Record::Recovery {
            turn,
            attempt,
            unknown,
        } => recover_record(session, *turn, *attempt, unknown)?,
        Record::Status {
            turn,
            last,
            outcome,
            result,
        } => {
            let log = running(session, *turn, "a status")?;
            log.finished = Some(Finished {
                outcome: *outcome,
                result: result.clone(),
                last: *last,
            });
        }
        Record::Event {
            turn,
            seq,
            at,
            delivery,
        } => event(session, *turn, *seq, *at, delivery)?,
        Record::Checkpoint {
            revision, payload, ..
        } => {
            let finished = session
                .turns
                .get(revision)
                .is_some_and(|log| log.finished.is_some());
            if !finished {
                return Err(format!(
                    "checkpoint at revision {revision}, which is not a finished turn"
                ));
            }
            session.checkpoints.insert(*revision, payload.clone());
        }
    }
    Ok(())
}
