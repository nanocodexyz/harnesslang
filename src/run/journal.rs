use std::collections::BTreeMap;

use crate::effect::EffectClass;
use crate::run::cell_journal::{CellUpdate, NESTED_ID_INFIX};
use crate::run::crc::crc32;
use crate::run::json::{self, Json};

pub const FIRST_SEQ: u64 = 1;
pub const CRC32_CHECK_INPUT: &[u8] = b"123456789";
pub const CRC32_CHECK: u32 = 0xcbf4_3926;
const LINE_PREFIX: &str = "{\"crc\":";
const LINE_MIDDLE: &str = ",\"record\":";
const LINE_SUFFIX: &str = "}";
pub const LINE_END: u8 = b'\n';

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
    Job(JobNotice),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobNotice {
    pub key: String,
    pub notice: String,
    pub done: bool,
}

impl Inbound {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Steer(_) => "steer",
            Self::Job(_) => "job",
        }
    }

    pub const fn interrupts(&self, interrupts: crate::ir::Interrupts) -> bool {
        match self {
            Self::Cancel => interrupts.cancel,
            Self::Steer(_) => interrupts.steer,
            Self::Job(_) => false,
        }
    }
}

pub const JOB_DONE: &str = "done";
pub const JOB_KEY_FIELD: &str = "key";
pub const LOST_PREFIX: &str = "lost:";

pub fn lost_notice(key: &str) -> String {
    format!(
        "<task-notification>\n<task-id>{key}</task-id>\n<status>killed</status>\n<summary>Background command {key} was still running when the harness that started it ended, so its completion was never reported and never will be. It was most likely stopped then; its outcome is unknown.</summary>\n</task-notification>\nRead its output file and check its effects before you rely on it or run it again."
    )
}

pub fn lost_delivery(key: &str) -> Delivery {
    Delivery {
        id: format!("{LOST_PREFIX}{key}"),
        inbound: Inbound::Job(JobNotice {
            key: key.to_owned(),
            notice: lost_notice(key),
            done: true,
        }),
    }
}

pub fn job_started(output: &Output) -> Option<&str> {
    match output {
        Output::Ok(Json::Object(map)) if map.len() == 1 => {
            map.get(crate::ir::JOB_KEY).and_then(Json::as_str)
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub id: String,
    pub inbound: Inbound,
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
        mode: Option<String>,
        settings: Option<Json>,
    },
    NestedCall {
        turn: u64,
        seq: u64,
        nested: u64,
        tool: String,
        effect: EffectClass,
        input: Json,
    },
    NestedResult {
        turn: u64,
        seq: u64,
        nested: u64,
        output: Output,
    },
    Cell {
        cell: u64,
        fresh: bool,
        session: String,
        update: CellUpdate,
    },
    Recovery {
        turn: u64,
        attempt: u64,
        unknown: Vec<u64>,
        nested: Vec<(u64, u64)>,
        replied: Vec<u64>,
        undispatched: Vec<u64>,
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
    Host {
        key: String,
        value: Json,
    },
}

impl Record {
    pub fn turn(&self) -> Option<u64> {
        match self {
            Self::Admitted { turn, .. }
            | Self::Call { turn, .. }
            | Self::Result { turn, .. }
            | Self::Model { turn, .. }
            | Self::Recovery { turn, .. }
            | Self::Status { turn, .. }
            | Self::NestedCall { turn, .. }
            | Self::NestedResult { turn, .. }
            | Self::Event { turn, .. } => Some(*turn),
            Self::Checkpoint { .. } | Self::Cell { .. } | Self::Host { .. } => None,
        }
    }
}

pub fn call_id(turn: u64, seq: u64) -> String {
    format!("{turn}/{seq}")
}

pub fn nested_id(turn: u64, seq: u64, nested: u64) -> String {
    crate::run::cell_journal::nested_call_id(&call_id(turn, seq), nested)
}

pub fn parse_any_call_id(text: &str) -> Result<(u64, u64, Option<u64>), String> {
    let Some((origin, nested)) = text.split_once(NESTED_ID_INFIX) else {
        let (turn, seq) = parse_call_id(text)?;
        return Ok((turn, seq, None));
    };
    let (turn, seq) = parse_call_id(origin)?;
    let nested = nested
        .parse()
        .map_err(|_| format!("call id {text:?} has no nested call number"))?;
    Ok((turn, seq, Some(nested)))
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

pub fn delivery_json(delivery: &Delivery) -> Json {
    let id = Json::text(&delivery.id);
    let fields = match &delivery.inbound {
        Inbound::Cancel => Json::object([("id", id)]),
        Inbound::Steer(message) => Json::object([
            ("id", id),
            (crate::check::STEER_PARAMETER, Json::text(message)),
        ]),
        Inbound::Job(job) => Json::object([
            (JOB_DONE, Json::Bool(job.done)),
            ("id", id),
            (JOB_KEY_FIELD, Json::text(&job.key)),
            (crate::check::JOB_PARAMETER, Json::text(&job.notice)),
        ]),
    };
    Json::object([(delivery.inbound.name(), fields)])
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
            fields.exact(&["id", crate::check::STEER_PARAMETER])?;
            Inbound::Steer(fields.text(crate::check::STEER_PARAMETER)?.to_owned())
        }
        "job" => {
            fields.exact(&[JOB_DONE, "id", JOB_KEY_FIELD, crate::check::JOB_PARAMETER])?;
            Inbound::Job(JobNotice {
                key: fields.text(JOB_KEY_FIELD)?.to_owned(),
                notice: fields.text(crate::check::JOB_PARAMETER)?.to_owned(),
                done: fields.boolean(JOB_DONE)?,
            })
        }
        other => return Err(format!("unknown event '{other}'")),
    };
    Ok(Delivery { id, inbound })
}

fn turn_text(turn: u64) -> Json {
    Json::String(turn.to_string())
}

pub fn output_json(output: &Output) -> Json {
    match output {
        Output::Ok(value) => Json::object([("ok", value.clone())]),
        Output::Failed(error) => Json::object([("failed", Json::text(error))]),
    }
}

fn status_json(turn: u64, last: u64, outcome: Outcome, result: &Json) -> Json {
    let finished = Json::object([
        ("last", Json::Number(last)),
        ("outcome", Json::text(outcome.name())),
        ("result", result.clone()),
    ]);
    Json::object([
        ("status", Json::object([("finished", finished)])),
        ("turn", turn_text(turn)),
    ])
}

pub fn to_json(record: &Record) -> Json {
    let (tag, body) = match record {
        Record::Admitted {
            turn,
            handler,
            arguments,
        } => (
            "admitted",
            Json::object([
                ("digest", Json::text(handler)),
                ("first", Json::Number(FIRST_SEQ)),
                ("hand", Json::Null),
                ("metadata", Json::object([("handler", Json::text(handler))])),
                ("prompt", arguments.clone()),
                ("turn", turn_text(*turn)),
            ]),
        ),
        Record::Call {
            turn,
            seq,
            tool,
            effect,
            input,
        } => (
            "call",
            Json::object([
                ("call_id", Json::String(call_id(*turn, *seq))),
                ("effect", Json::text(effect.name())),
                ("input", input.clone()),
                ("seq", Json::Number(*seq)),
                ("tool", Json::text(tool)),
                ("turn", turn_text(*turn)),
            ]),
        ),
        Record::Result { turn, seq, output } => (
            "result",
            Json::object([
                ("call_id", Json::String(call_id(*turn, *seq))),
                ("output", output_json(output)),
                ("seq", Json::Number(*seq)),
            ]),
        ),
        Record::Model {
            turn,
            seq,
            response,
            mode,
            settings,
        } => {
            let mut fields = BTreeMap::from([
                ("call_id".to_owned(), Json::String(call_id(*turn, *seq))),
                ("response".to_owned(), response.clone()),
                ("seq".to_owned(), Json::Number(*seq)),
                ("turn".to_owned(), turn_text(*turn)),
            ]);
            if let Some(mode) = mode {
                fields.insert(MODEL_MODE.to_owned(), Json::text(mode));
            }
            if let Some(settings) = settings {
                fields.insert(MODEL_SETTINGS.to_owned(), settings.clone());
            }
            ("model", Json::Object(fields))
        }
        Record::Recovery {
            turn,
            attempt,
            unknown,
            nested,
            replied,
            undispatched,
        } => {
            let mut fields = BTreeMap::from([
                ("attempt".to_owned(), Json::Number(*attempt)),
                ("turn".to_owned(), turn_text(*turn)),
                ("unknown".to_owned(), recovery_ids(*turn, unknown, nested)),
            ]);
            if !replied.is_empty() {
                fields.insert(
                    RECOVERY_REPLIED.to_owned(),
                    recovery_ids(*turn, replied, &[]),
                );
            }
            if !undispatched.is_empty() {
                fields.insert(
                    RECOVERY_UNDISPATCHED.to_owned(),
                    recovery_ids(*turn, undispatched, &[]),
                );
            }
            ("recovery", Json::Object(fields))
        }
        Record::NestedCall {
            turn,
            seq,
            nested,
            tool,
            effect,
            input,
        } => (
            "call",
            Json::object([
                ("call_id", Json::String(nested_id(*turn, *seq, *nested))),
                ("effect", Json::text(effect.name())),
                ("input", input.clone()),
                ("seq", Json::Number(*seq)),
                ("tool", Json::text(tool)),
                ("turn", turn_text(*turn)),
            ]),
        ),
        Record::NestedResult {
            turn,
            seq,
            nested,
            output,
        } => (
            "result",
            Json::object([
                ("call_id", Json::String(nested_id(*turn, *seq, *nested))),
                ("output", output_json(output)),
                ("seq", Json::Number(*seq)),
            ]),
        ),
        Record::Cell {
            cell,
            fresh,
            session,
            update,
        } => (
            "cell",
            Json::object([
                ("cell", Json::Number(*cell)),
                ("fresh", Json::Bool(*fresh)),
                ("session", Json::text(session)),
                ("update", update.to_json()),
            ]),
        ),
        Record::Status {
            turn,
            last,
            outcome,
            result,
        } => ("status", status_json(*turn, *last, *outcome, result)),
        Record::Checkpoint {
            revision,
            state,
            payload,
        } => (
            "checkpoint",
            Json::object([
                ("payload", Json::String(payload.clone())),
                ("revision", Json::Number(*revision)),
                ("state", Json::text(state)),
            ]),
        ),
        Record::Event {
            turn,
            seq,
            at,
            delivery,
        } => (
            "event",
            Json::object([
                ("at", Json::Number(*at)),
                ("event", delivery_json(delivery)),
                ("seq", Json::Number(*seq)),
                ("turn", turn_text(*turn)),
            ]),
        ),
        Record::Host { key, value } => (
            "host",
            Json::object([("key", Json::text(key)), ("value", value.clone())]),
        ),
    };
    Json::object([(tag, body)])
}

fn recovery_ids(turn: u64, unknown: &[u64], nested: &[(u64, u64)]) -> Json {
    assert!(
        unknown.windows(2).all(|pair| pair[0] < pair[1]),
        "a recovery lists its top-level calls in seq order"
    );
    assert!(
        nested.windows(2).all(|pair| pair[0] < pair[1]),
        "a recovery lists its nested calls in order"
    );
    let mut keyed: Vec<((u64, u64), String)> = unknown
        .iter()
        .map(|seq| ((*seq, 0), call_id(turn, *seq)))
        .chain(
            nested
                .iter()
                .map(|(seq, id)| ((*seq, *id), nested_id(turn, *seq, *id))),
        )
        .collect();
    keyed.sort();
    Json::Array(keyed.into_iter().map(|(_, id)| Json::String(id)).collect())
}

pub const MODEL_MODE: &str = "mode";
pub const MODEL_SETTINGS: &str = "settings";
pub const RECOVERY_REPLIED: &str = "replied";
pub const RECOVERY_UNDISPATCHED: &str = "not_dispatched";

fn attested_seqs(fields: &Fields<'_>, key: &str, turn: u64) -> Result<Vec<u64>, String> {
    let Some(json) = fields.map.get(key) else {
        return Ok(Vec::new());
    };
    let Json::Array(items) = json else {
        return Err(format!("recovery record's {key} is not a list"));
    };
    let mut seqs = Vec::with_capacity(items.len());
    for item in items {
        let id = item
            .as_str()
            .ok_or_else(|| format!("recovery's {key} lists a call id that is not text"))?;
        let (call_turn, seq) = parse_call_id(id)?;
        if call_turn != turn {
            return Err(format!("recovery's {key} lists a call of another turn"));
        }
        seqs.push(seq);
    }
    if seqs.is_empty() || seqs.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(format!(
            "recovery's {key} lists no calls, or lists them out of order"
        ));
    }
    Ok(seqs)
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

    fn call(&self) -> Result<(u64, u64, Option<u64>), String> {
        let (turn, seq, nested) = parse_any_call_id(self.text("call_id")?)?;
        if self.number("seq")? != seq {
            return Err(format!(
                "{} record's seq disagrees with its call id",
                self.tag
            ));
        }
        Ok((turn, seq, nested))
    }

    fn boolean(&self, key: &str) -> Result<bool, String> {
        match self.get(key)? {
            Json::Bool(value) => Ok(*value),
            _ => Err(format!("{} record's '{key}' is not a bool", self.tag)),
        }
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

fn output_from(json: &Json) -> Result<Output, String> {
    match json.tagged() {
        Some(("ok", value)) => Ok(Output::Ok(value.clone())),
        Some(("failed", Json::String(error))) => Ok(Output::Failed(error.clone())),
        _ => Err(format!("output {} is neither ok nor failed", json.encode())),
    }
}

pub fn from_json(json: &Json) -> Result<Record, String> {
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
        "call" => call_from(&fields),
        "result" => {
            fields.exact(&["call_id", "output", "seq"])?;
            let (turn, seq, nested) = fields.call()?;
            let output = output_from(fields.get("output")?)?;
            Ok(match nested {
                None => Record::Result { turn, seq, output },
                Some(nested) => Record::NestedResult {
                    turn,
                    seq,
                    nested,
                    output,
                },
            })
        }
        "cell" => cell_from(&fields),
        "model" => {
            let mut keys = vec!["call_id", "response", "seq", "turn"];
            let mode = fields.map.contains_key(MODEL_MODE);
            let settings = fields.map.contains_key(MODEL_SETTINGS);
            keys.extend(mode.then_some(MODEL_MODE));
            keys.extend(settings.then_some(MODEL_SETTINGS));
            fields.exact(&keys)?;
            let mode = if mode {
                Some(fields.text(MODEL_MODE)?.to_owned())
            } else {
                None
            };
            let settings = if settings {
                let value = fields.get(MODEL_SETTINGS)?;
                if !matches!(value, Json::Object(_)) {
                    return Err(format!(
                        "a model record's {MODEL_SETTINGS} is not an object"
                    ));
                }
                Some(value.clone())
            } else {
                None
            };
            let (turn, seq) = top_level(fields.call()?)?;
            Ok(Record::Model {
                turn,
                seq,
                response: fields.get("response")?.clone(),
                mode,
                settings,
            })
        }
        "recovery" => {
            let mut keys = vec!["attempt", "turn", "unknown"];
            for key in [RECOVERY_UNDISPATCHED, RECOVERY_REPLIED] {
                if fields.map.contains_key(key) {
                    keys.push(key);
                }
            }
            keys.sort_unstable();
            fields.exact(&keys)?;
            let turn = fields.turn()?;
            let Json::Array(items) = fields.get("unknown")? else {
                return Err("recovery record's unknown is not a list".to_owned());
            };
            let mut unknown = Vec::new();
            let mut nested = Vec::new();
            for item in items {
                let (call_turn, seq, inner) = parse_any_call_id(
                    item.as_str()
                        .ok_or("recovery lists a call id that is not text")?,
                )?;
                if call_turn != turn {
                    return Err("recovery lists a call of another turn".to_owned());
                }
                match inner {
                    None => unknown.push(seq),
                    Some(inner) => nested.push((seq, inner)),
                }
            }
            unknown.sort_unstable();
            nested.sort_unstable();
            if unknown.windows(2).any(|pair| pair[0] == pair[1])
                || nested.windows(2).any(|pair| pair[0] == pair[1])
            {
                return Err("recovery lists a call twice".to_owned());
            }
            let record = Record::Recovery {
                turn,
                attempt: fields.number("attempt")?,
                unknown,
                nested,
                replied: attested_seqs(&fields, RECOVERY_REPLIED, turn)?,
                undispatched: attested_seqs(&fields, RECOVERY_UNDISPATCHED, turn)?,
            };
            if to_json(&record)
                .get("recovery")
                .and_then(|body| body.get("unknown"))
                != Some(fields.get("unknown")?)
            {
                return Err("recovery lists its calls out of order".to_owned());
            }
            Ok(record)
        }
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
        "host" => {
            fields.exact(&["key", "value"])?;
            Ok(Record::Host {
                key: fields.text("key")?.to_owned(),
                value: fields.get("value")?.clone(),
            })
        }
        other => Err(format!("unknown record '{other}'")),
    }
}

fn top_level(id: (u64, u64, Option<u64>)) -> Result<(u64, u64), String> {
    match id {
        (turn, seq, None) => Ok((turn, seq)),
        (_, _, Some(_)) => Err("a model reply has a nested call id".to_owned()),
    }
}

fn call_from(fields: &Fields<'_>) -> Result<Record, String> {
    fields.exact(&["call_id", "effect", "input", "seq", "tool", "turn"])?;
    let (turn, seq, nested) = fields.call()?;
    if fields.turn()? != turn {
        return Err("call record's turn disagrees with its call id".to_owned());
    }
    let effect = EffectClass::parse(fields.text("effect")?)
        .ok_or("call record has an unknown effect class")?;
    let tool = fields.text("tool")?.to_owned();
    let input = fields.get("input")?.clone();
    Ok(match nested {
        None => Record::Call {
            turn,
            seq,
            tool,
            effect,
            input,
        },
        Some(nested) => Record::NestedCall {
            turn,
            seq,
            nested,
            tool,
            effect,
            input,
        },
    })
}

fn cell_from(fields: &Fields<'_>) -> Result<Record, String> {
    fields.exact(&["cell", "fresh", "session", "update"])?;
    let cell = fields.number("cell")?;
    let fresh = fields.boolean("fresh")?;
    let session = fields.text("session")?.to_owned();
    let update = CellUpdate::from_json(fields.get("update")?)?;
    if update.cell_id != cell || update.session_id != session {
        return Err("cell record's cell or session disagrees with its update".to_owned());
    }
    if update.header.is_some() != fresh {
        return Err("cell record is fresh exactly when its update carries the header".to_owned());
    }
    Ok(Record::Cell {
        cell,
        fresh,
        session,
        update,
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
    let body = to_json(record).encode();
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
    from_json(&json::decode(body)?)
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

pub fn decode_lines(bytes: &[u8], checksum: bool) -> Result<Decoded, String> {
    let mut records = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let Some(length) = bytes[offset..].iter().position(|byte| *byte == LINE_END) else {
            if !checksum {
                let text = String::from_utf8_lossy(&bytes[offset..]);
                return Err(
                    format!("line {} is not terminated: {text:?}", records.len() + 1)
                        .chars()
                        .take(160)
                        .collect(),
                );
            }
            break;
        };
        let line = &bytes[offset..offset + length];
        let decoded = std::str::from_utf8(line)
            .map_err(|_| "line is not utf-8".to_owned())
            .and_then(|line| decode_line(line, checksum));
        match decoded {
            Ok(record) => records.push(record),
            Err(_) if checksum => break,
            Err(error) => return Err(format!("line {}: {error}", records.len() + 1)),
        }
        offset += length + 1;
    }
    Ok(Decoded {
        records,
        valid_bytes: offset,
        dropped_bytes: bytes.len() - offset,
    })
}
