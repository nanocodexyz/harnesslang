use std::collections::BTreeMap;

use crate::run::json::Json;

pub const CELL_JOURNAL_VERSION: u64 = 1;
pub const JOURNAL_LIMIT_BYTES: u64 = 32 * 1024 * 1024;
pub const ENTRY_BYTES: u64 = 32;
pub const CALL_INTERRUPTED: &str = "CODE_MODE_CALL_INTERRUPTED";
pub const CALL_LIMIT: &str = "call_limit";
pub const UNKNOWN_OUTCOME: &str = "This nested call was running when its cell moved to another host; its outcome is unknown. Do not assume it did not run or automatically repeat it.";
pub const NESTED_ID_INFIX: &str = "/code-";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Call {
        id: u64,
        name: String,
        input: Json,
    },
    Result {
        id: u64,
        encoded: String,
        success: bool,
    },
    Timeout {
        id: u64,
        delay_ms: u64,
        at_ms: u64,
    },
    Timer {
        id: u64,
    },
    Clock {
        value: u64,
    },
    Monotonic {
        value: u64,
    },
    Random {
        value: u32,
    },
    Bytes {
        data: Vec<u8>,
    },
}

impl Entry {
    pub const fn is_input(&self) -> bool {
        matches!(self, Self::Result { .. } | Self::Timer { .. })
    }

    pub fn same_step(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Timeout {
                    id: left,
                    delay_ms: left_delay,
                    ..
                },
                Self::Timeout {
                    id: right,
                    delay_ms: right_delay,
                    ..
                },
            ) => left == right && left_delay == right_delay,
            (Self::Clock { .. }, Self::Clock { .. })
            | (Self::Monotonic { .. }, Self::Monotonic { .. })
            | (Self::Random { .. }, Self::Random { .. }) => true,
            (Self::Bytes { data: left }, Self::Bytes { data: right }) => left.len() == right.len(),
            (left, right) => left == right,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Call { name, id, .. } => format!("call {name} (nested call {id})"),
            Self::Result { id, .. } => format!("the result of nested call {id}"),
            Self::Timeout { id, delay_ms, .. } => format!("timer {id} set for {delay_ms} ms"),
            Self::Timer { id } => format!("timer {id} firing"),
            Self::Clock { .. } => "a clock read".to_owned(),
            Self::Monotonic { .. } => "a performance.now() read".to_owned(),
            Self::Random { .. } => "a random number".to_owned(),
            Self::Bytes { data } => format!("{} random bytes", data.len()),
        }
    }

    pub fn to_json(&self) -> Json {
        let kind = |name: &str| ("kind", Json::text(name));
        match self {
            Self::Call { id, name, input } => Json::object([
                kind("call"),
                ("host", Json::Bool(false)),
                ("id", Json::Number(*id)),
                ("input", input.clone()),
                ("name", Json::text(name)),
            ]),
            Self::Result {
                id,
                encoded,
                success,
            } => Json::object([
                kind("result"),
                ("encoded", Json::text(encoded)),
                ("id", Json::Number(*id)),
                ("success", Json::Bool(*success)),
            ]),
            Self::Timeout {
                id,
                delay_ms,
                at_ms,
            } => Json::object([
                kind("timeout"),
                ("at_ms", Json::Number(*at_ms)),
                ("delay_ms", Json::Number(*delay_ms)),
                ("id", Json::Number(*id)),
            ]),
            Self::Timer { id } => Json::object([kind("timer"), ("id", Json::Number(*id))]),
            Self::Clock { value } => Json::object([kind("clock"), ("value", Json::Number(*value))]),
            Self::Monotonic { value } => {
                Json::object([kind("monotonic"), ("value", Json::Number(*value))])
            }
            Self::Random { value } => {
                Json::object([kind("random"), ("value", Json::Fraction(*value))])
            }
            Self::Bytes { data } => Json::object([
                kind("bytes"),
                (
                    "data",
                    Json::Array(
                        data.iter()
                            .map(|byte| Json::Number(u64::from(*byte)))
                            .collect(),
                    ),
                ),
            ]),
        }
    }

    pub fn from_json(json: &Json) -> Result<Self, String> {
        let Json::Object(map) = json else {
            return Err("a cell journal entry is an object".to_owned());
        };
        let fields = Fields { what: "entry", map };
        Ok(match fields.text("kind")? {
            "call" => {
                fields.exact(&["host", "id", "input", "kind", "name"])?;
                fields.no_host()?;
                Self::Call {
                    id: fields.number("id")?,
                    name: fields.text("name")?.to_owned(),
                    input: fields.get("input")?.clone(),
                }
            }
            "result" => {
                fields.exact(&["encoded", "id", "kind", "success"])?;
                Self::Result {
                    id: fields.number("id")?,
                    encoded: fields.text("encoded")?.to_owned(),
                    success: fields.boolean("success")?,
                }
            }
            "timeout" => {
                fields.exact(&["at_ms", "delay_ms", "id", "kind"])?;
                Self::Timeout {
                    id: fields.number("id")?,
                    delay_ms: fields.number("delay_ms")?,
                    at_ms: fields.number("at_ms")?,
                }
            }
            "timer" => {
                fields.exact(&["id", "kind"])?;
                Self::Timer {
                    id: fields.number("id")?,
                }
            }
            other => return scalar_entry(&fields, other),
        })
    }

    pub fn bytes(&self) -> u64 {
        self.to_json().encode().len() as u64 + ENTRY_BYTES
    }
}

fn scalar_entry(fields: &Fields<'_>, kind: &str) -> Result<Entry, String> {
    fields.exact(&["kind", if kind == "bytes" { "data" } else { "value" }])?;
    Ok(match kind {
        "clock" => Entry::Clock {
            value: fields.number("value")?,
        },
        "monotonic" => Entry::Monotonic {
            value: fields.number("value")?,
        },
        "random" => match fields.get("value")? {
            Json::Fraction(value) => Entry::Random { value: *value },
            other => {
                return Err(format!(
                    "random entry value {} is not a fraction",
                    other.encode()
                ));
            }
        },
        "bytes" => {
            let Json::Array(items) = fields.get("data")? else {
                return Err("bytes entry data is not a list".to_owned());
            };
            let mut data = Vec::with_capacity(items.len());
            for item in items {
                let byte = item
                    .as_u64()
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or("bytes entry holds a value that is not a byte")?;
                data.push(byte);
            }
            Entry::Bytes { data }
        }
        other => return Err(format!("unknown cell journal entry kind '{other}'")),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallState {
    Queued,
    Running,
    Completed { encoded: String, success: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournaledCall {
    pub id: u64,
    pub call_id: String,
    pub name: String,
    pub input: Json,
    pub state: CallState,
}

impl JournaledCall {
    pub fn to_json(&self) -> Json {
        let state = match &self.state {
            CallState::Queued => Json::object([("state", Json::text("queued"))]),
            CallState::Running => Json::object([("state", Json::text("running"))]),
            CallState::Completed { encoded, success } => Json::object([
                ("encoded", Json::text(encoded)),
                ("state", Json::text("completed")),
                ("success", Json::Bool(*success)),
            ]),
        };
        Json::object([
            ("call_id", Json::text(&self.call_id)),
            ("host", Json::Bool(false)),
            ("id", Json::Number(self.id)),
            ("input", self.input.clone()),
            ("name", Json::text(&self.name)),
            ("state", state),
        ])
    }

    pub fn from_json(json: &Json) -> Result<Self, String> {
        let Json::Object(map) = json else {
            return Err("a journaled call is an object".to_owned());
        };
        let fields = Fields { what: "call", map };
        fields.exact(&["call_id", "host", "id", "input", "name", "state"])?;
        fields.no_host()?;
        let Json::Object(state) = fields.get("state")? else {
            return Err("a journaled call's state is an object".to_owned());
        };
        let state_fields = Fields {
            what: "call state",
            map: state,
        };
        let state = match state_fields.text("state")? {
            "queued" => {
                state_fields.exact(&["state"])?;
                CallState::Queued
            }
            "running" => {
                state_fields.exact(&["state"])?;
                CallState::Running
            }
            "completed" => {
                state_fields.exact(&["encoded", "state", "success"])?;
                CallState::Completed {
                    encoded: state_fields.text("encoded")?.to_owned(),
                    success: state_fields.boolean("success")?,
                }
            }
            other => return Err(format!("unknown call state '{other}'")),
        };
        Ok(Self {
            id: fields.number("id")?,
            call_id: fields.text("call_id")?.to_owned(),
            name: fields.text("name")?.to_owned(),
            input: fields.get("input")?.clone(),
            state,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub origin_call_id: String,
    pub script: String,
    pub tools: Vec<Json>,
    pub yield_time_ms: u64,
}

impl Header {
    pub fn to_json(&self) -> Json {
        Json::object([
            ("max_output_tokens", Json::Null),
            ("origin_call_id", Json::text(&self.origin_call_id)),
            ("prelude", Json::Bool(false)),
            ("script", Json::text(&self.script)),
            ("stored", Json::Object(BTreeMap::new())),
            ("tools", Json::Array(self.tools.clone())),
            ("version", Json::Number(CELL_JOURNAL_VERSION)),
            ("yield_time_ms", Json::Number(self.yield_time_ms)),
        ])
    }

    pub fn from_json(json: &Json) -> Result<Self, String> {
        let Json::Object(map) = json else {
            return Err("a cell header is an object".to_owned());
        };
        let fields = Fields {
            what: "header",
            map,
        };
        fields.exact(&[
            "max_output_tokens",
            "origin_call_id",
            "prelude",
            "script",
            "stored",
            "tools",
            "version",
            "yield_time_ms",
        ])?;
        if fields.number("version")? != CELL_JOURNAL_VERSION {
            return Err(format!(
                "cell header version {} is not {CELL_JOURNAL_VERSION}",
                fields.number("version")?
            ));
        }
        if fields.get("max_output_tokens")? != &Json::Null
            || fields.get("prelude")? != &Json::Bool(false)
            || fields.get("stored")? != &Json::Object(BTreeMap::new())
        {
            return Err(
                "cell header carries stored values, a prelude or max_output_tokens".to_owned(),
            );
        }
        let Json::Array(tools) = fields.get("tools")? else {
            return Err("cell header tools is not a list".to_owned());
        };
        Ok(Self {
            origin_call_id: fields.text("origin_call_id")?.to_owned(),
            script: fields.text("script")?.to_owned(),
            tools: tools.clone(),
            yield_time_ms: fields.number("yield_time_ms")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellUpdate {
    pub session_id: String,
    pub cell_id: u64,
    pub seq: u64,
    pub header: Option<Header>,
    pub entries_from: u64,
    pub entries: Vec<Entry>,
    pub calls: Vec<JournaledCall>,
    pub finished: bool,
}

impl CellUpdate {
    pub fn to_json(&self) -> Json {
        let mut map = BTreeMap::new();
        map.insert("session_id".to_owned(), Json::text(&self.session_id));
        map.insert("cell_id".to_owned(), Json::Number(self.cell_id));
        map.insert("seq".to_owned(), Json::Number(self.seq));
        if let Some(header) = &self.header {
            map.insert("header".to_owned(), header.to_json());
        }
        map.insert("entries_from".to_owned(), Json::Number(self.entries_from));
        map.insert(
            "entries".to_owned(),
            Json::Array(self.entries.iter().map(Entry::to_json).collect()),
        );
        map.insert(
            "calls".to_owned(),
            Json::Array(self.calls.iter().map(JournaledCall::to_json).collect()),
        );
        map.insert("content_returned".to_owned(), Json::Number(0));
        if self.finished {
            map.insert("finished".to_owned(), Json::Bool(true));
        }
        Json::Object(map)
    }

    pub fn from_json(json: &Json) -> Result<Self, String> {
        let Json::Object(map) = json else {
            return Err("a cell update is an object".to_owned());
        };
        let fields = Fields {
            what: "cell update",
            map,
        };
        let mut keys = vec![
            "calls",
            "cell_id",
            "content_returned",
            "entries",
            "entries_from",
            "seq",
            "session_id",
        ];
        let header = match map.get("header") {
            Some(header) => {
                keys.push("header");
                Some(Header::from_json(header)?)
            }
            None => None,
        };
        let finished = match map.get("finished") {
            Some(Json::Bool(true)) => {
                keys.push("finished");
                true
            }
            Some(_) => return Err("cell update's finished is written only when true".to_owned()),
            None => false,
        };
        keys.sort_unstable();
        fields.exact(&keys)?;
        if fields.number("content_returned")? != 0 {
            return Err("cell update returns content, which harnesslang never writes".to_owned());
        }
        Ok(Self {
            session_id: fields.text("session_id")?.to_owned(),
            cell_id: fields.number("cell_id")?,
            seq: fields.number("seq")?,
            header,
            entries_from: fields.number("entries_from")?,
            entries: fields.list("entries", Entry::from_json)?,
            calls: fields.list("calls", JournaledCall::from_json)?,
            finished,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellLog {
    pub cell: u64,
    pub header: Header,
    pub entries: Vec<Entry>,
    pub calls: BTreeMap<u64, JournaledCall>,
    pub finished: bool,
    pub next_seq: u64,
}

impl CellLog {
    pub fn start(update: &CellUpdate) -> Result<Self, String> {
        let Some(header) = &update.header else {
            return Err(format!("cell {} starts without a header", update.cell_id));
        };
        if update.seq != 0 || update.entries_from != 0 {
            return Err(format!(
                "cell {}'s header update has seq {} and entries_from {}",
                update.cell_id, update.seq, update.entries_from
            ));
        }
        let mut log = Self {
            cell: update.cell_id,
            header: header.clone(),
            entries: Vec::new(),
            calls: BTreeMap::new(),
            finished: false,
            next_seq: 0,
        };
        log.apply(update)?;
        Ok(log)
    }

    pub fn apply(&mut self, update: &CellUpdate) -> Result<(), String> {
        let cell = self.cell;
        if update.cell_id != cell {
            return Err(format!(
                "cell {cell} folds an update of cell {}",
                update.cell_id
            ));
        }
        if self.finished {
            return Err(format!("cell {cell} has an update after it finished"));
        }
        if update.seq != self.next_seq {
            return Err(format!(
                "cell {cell} update seq {} follows seq {}",
                update.seq,
                self.next_seq.wrapping_sub(1)
            ));
        }
        if update.entries_from != self.entries.len() as u64 {
            return Err(format!(
                "cell {cell} update starts at entry {} after {} entries",
                update.entries_from,
                self.entries.len()
            ));
        }
        if update.header.is_some() != (update.seq == 0) {
            return Err(format!(
                "cell {cell} carries its header on an update other than its first"
            ));
        }
        self.next_seq += 1;
        self.entries.extend(update.entries.iter().cloned());
        for call in &update.calls {
            self.calls.insert(call.id, call.clone());
        }
        self.finished = update.finished;
        Ok(())
    }

    pub fn delivered(&self, id: u64) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry, Entry::Result { id: delivered, .. } if *delivered == id))
    }

    pub fn unknown_calls(&self) -> Vec<u64> {
        self.calls
            .values()
            .filter(|call| call.state == CallState::Running && !self.delivered(call.id))
            .map(|call| call.id)
            .collect()
    }

    pub fn bytes(&self) -> u64 {
        self.entries.iter().map(Entry::bytes).sum()
    }
}

pub fn nested_call_id(origin: &str, id: u64) -> String {
    format!("{origin}{NESTED_ID_INFIX}{id}")
}

pub fn unknown_outcome() -> Json {
    Json::object([
        ("code", Json::text(CALL_INTERRUPTED)),
        ("error", Json::text(UNKNOWN_OUTCOME)),
        ("outcome", Json::text("unknown")),
    ])
}

pub fn divergence_text(cell_id: u64, reason: &str) -> String {
    format!(
        "This cell moved to another host and could not be resumed ({reason}), so it stopped and its outcome is unknown: nested calls it made may have run. Cell {cell_id} no longer exists: don't wait on it again."
    )
}

struct Fields<'a> {
    what: &'static str,
    map: &'a BTreeMap<String, Json>,
}

impl<'a> Fields<'a> {
    fn get(&self, key: &str) -> Result<&'a Json, String> {
        self.map
            .get(key)
            .ok_or_else(|| format!("{} lacks '{key}'", self.what))
    }

    fn number(&self, key: &str) -> Result<u64, String> {
        self.get(key)?
            .as_u64()
            .ok_or_else(|| format!("{}'s '{key}' is not a number", self.what))
    }

    fn text(&self, key: &str) -> Result<&'a str, String> {
        self.get(key)?
            .as_str()
            .ok_or_else(|| format!("{}'s '{key}' is not text", self.what))
    }

    fn boolean(&self, key: &str) -> Result<bool, String> {
        match self.get(key)? {
            Json::Bool(value) => Ok(*value),
            _ => Err(format!("{}'s '{key}' is not a bool", self.what)),
        }
    }

    fn no_host(&self) -> Result<(), String> {
        if self.boolean("host")? {
            return Err(format!(
                "{} runs on a host, which harnesslang never routes to",
                self.what
            ));
        }
        Ok(())
    }

    fn list<T>(&self, key: &str, item: fn(&Json) -> Result<T, String>) -> Result<Vec<T>, String> {
        let Json::Array(items) = self.get(key)? else {
            return Err(format!("{}'s '{key}' is not a list", self.what));
        };
        items.iter().map(item).collect()
    }

    fn exact(&self, keys: &[&str]) -> Result<(), String> {
        if self.map.len() != keys.len() || keys.iter().any(|key| !self.map.contains_key(*key)) {
            return Err(format!(
                "{} has fields {:?}, expected {keys:?}",
                self.what,
                self.map.keys().collect::<Vec<_>>()
            ));
        }
        Ok(())
    }
}
