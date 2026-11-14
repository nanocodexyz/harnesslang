use std::collections::BTreeSet;
use std::path::Path;

use crc::{CRC_32_ISO_HDLC, Crc};
use nanocode_code_mode_engine::JournalEntry;
use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::cells;
use crate::djs::Terminal;
use crate::golden::{self, Lived, NESTED_TOOLS};

const CHECKSUM: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);
const SESSION: &str = "synthetic";
const TURN: u64 = 1;
const MODEL_SEQ: u64 = 1;
const SITE_SEQ: u64 = 2;
const CELL: u64 = 1;
const CODE_TOOL: &str = "exec";
const HANDLER: &str = "ask";
const RECOVERY_ATTEMPT: u64 = 1;
const YIELD_MS: u64 = 10_000;
const FRACTION_BITS: u32 = 32;
const FRACTION_DIGITS: usize = 32;
const FRACTION_SCALE: u128 = 5_u128.pow(FRACTION_BITS);
const SEED: u64 = 0;
const TARGETS: [(&str, bool); 3] = [
    ("native-jsonl", true),
    ("server-postgres", false),
    ("vercel", false),
];
const BROKEN_TARGET: (&str, bool) = TARGETS[0];
const TAMPERED_TEXT: &str = "a site result djs does not print";
const PLAIN_FAILURE: &str = "{\"error\":\"interrupted\"}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Entry(usize),
    NestedCall,
    SiteResult,
    Other,
}

#[derive(Clone)]
struct Item {
    kind: Kind,
    body: String,
}

#[derive(Serialize)]
struct UpdateOut<'a> {
    calls: Vec<Value>,
    cell_id: u64,
    content_returned: u64,
    entries: Vec<&'a RawValue>,
    entries_from: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    header: Option<Value>,
    seq: u64,
    session_id: &'a str,
}

#[derive(Serialize)]
struct CellOut<'a> {
    cell: u64,
    fresh: bool,
    session: &'a str,
    update: &'a RawValue,
}

#[derive(Serialize)]
struct CellRecord<'a> {
    cell: CellOut<'a>,
}

struct Shaped {
    entry: JournalEntry,
    text: Box<RawValue>,
}

fn integral(value: f64, what: &str) -> Result<u64, String> {
    if value < 0.0 || value.fract() != 0.0 {
        return Err(format!("{what} value {value} is not an unsigned integer"));
    }
    Ok(value as u64)
}

fn fraction(value: f64) -> Result<(f64, String), String> {
    if !(0.0..1.0).contains(&value) {
        return Err(format!("random value {value} is outside [0, 1)"));
    }
    let numerator = (value * f64::from(2_u32).powi(FRACTION_BITS as i32)) as u64;
    let exact = numerator as f64 / f64::from(2_u32).powi(FRACTION_BITS as i32);
    let digits = format!(
        "{:0width$}",
        u128::from(numerator) * FRACTION_SCALE,
        width = FRACTION_DIGITS
    );
    let trimmed = digits.trim_end_matches('0');
    let shown = if trimmed.is_empty() { "0" } else { trimmed };
    Ok((exact, format!("0.{shown}")))
}

fn raw(text: String) -> Box<RawValue> {
    RawValue::from_string(text).expect("the oracle writes JSON")
}

fn shaped(entry: &JournalEntry) -> Result<Shaped, String> {
    let (entry, text) = match entry {
        JournalEntry::Clock { value } => {
            let value = integral(*value, "clock")?;
            (
                JournalEntry::Clock {
                    value: value as f64,
                },
                json!({"kind": "clock", "value": value}).to_string(),
            )
        }
        JournalEntry::Monotonic { value } => {
            let value = integral(*value, "monotonic")?;
            (
                JournalEntry::Monotonic {
                    value: value as f64,
                },
                json!({"kind": "monotonic", "value": value}).to_string(),
            )
        }
        JournalEntry::Random { value } => {
            let (value, text) = fraction(*value)?;
            (
                JournalEntry::Random { value },
                format!("{{\"kind\":\"random\",\"value\":{text}}}"),
            )
        }
        other => (
            other.clone(),
            serde_json::to_string(other).map_err(|error| error.to_string())?,
        ),
    };
    Ok(Shaped {
        entry,
        text: raw(text),
    })
}

fn external(name: &str) -> bool {
    NESTED_TOOLS
        .iter()
        .any(|(tool, class)| *tool == name && *class == "external")
}

fn origin() -> String {
    format!("{TURN}/{SITE_SEQ}")
}

fn nested_id(id: u64) -> String {
    format!("{}/code-{id}", origin())
}

fn header(lived: &Lived) -> Value {
    json!({
        "max_output_tokens": null,
        "origin_call_id": origin(),
        "prelude": false,
        "script": lived.cell.script,
        "stored": {},
        "tools": lived.cell.tools,
        "version": 1,
        "yield_time_ms": YIELD_MS,
    })
}

struct Writer<'a> {
    lived: &'a Lived,
    items: Vec<Item>,
    seq: u64,
    entries: usize,
    inputs: Vec<(u64, String, Value)>,
}

impl Writer<'_> {
    fn other(&mut self, kind: Kind, value: Value) {
        self.items.push(Item {
            kind,
            body: value.to_string(),
        });
    }

    fn update(&mut self, kind: Kind, entry: Option<&Shaped>, calls: Vec<Value>, finished: bool) {
        let fresh = self.seq == 0;
        let update = UpdateOut {
            calls,
            cell_id: CELL,
            content_returned: 0,
            entries: entry.iter().map(|shaped| shaped.text.as_ref()).collect(),
            entries_from: self.entries,
            finished: finished.then_some(true),
            header: fresh.then(|| header(self.lived)),
            seq: self.seq,
            session_id: SESSION,
        };
        let update = raw(serde_json::to_string(&update).expect("an update serializes"));
        let record = CellRecord {
            cell: CellOut {
                cell: CELL,
                fresh,
                session: SESSION,
                update: &update,
            },
        };
        self.items.push(Item {
            kind,
            body: serde_json::to_string(&record).expect("a cell record serializes"),
        });
        self.seq += 1;
        self.entries += usize::from(entry.is_some());
    }

    fn call(&self, id: u64, state: Value) -> Result<Value, String> {
        let (_, name, input) = self
            .inputs
            .iter()
            .find(|(found, _, _)| *found == id)
            .ok_or_else(|| format!("a result for nested call {id}, which has no Call entry"))?;
        Ok(json!({
            "call_id": nested_id(id),
            "host": false,
            "id": id,
            "input": input,
            "name": name,
            "state": state,
        }))
    }
}

fn site_output(terminal: &Terminal, text: &str) -> Result<(Value, &'static str, Value), String> {
    match terminal {
        Terminal::Done => Ok((json!({"ok": text}), "completed", json!(text))),
        Terminal::Threw(error) => Ok((json!({"failed": error}), "failed", json!(error))),
        other => Err(format!("a synthetic cell must finish, not {other:?}")),
    }
}

fn opening(writer: &mut Writer<'_>) {
    writer.other(
        Kind::Other,
        json!({"admitted": {"digest": HANDLER, "first": MODEL_SEQ, "hand": null, "metadata": {"handler": HANDLER}, "prompt": {"question": writer.lived.name}, "turn": TURN.to_string()}}),
    );
    writer.other(
        Kind::Other,
        json!({"model": {"call_id": format!("{TURN}/{MODEL_SEQ}"), "response": {"calls": [{"name": CODE_TOOL}]}, "seq": MODEL_SEQ, "turn": TURN.to_string()}}),
    );
    writer.other(
        Kind::Other,
        json!({"call": {"call_id": origin(), "effect": "none", "input": writer.lived.cell.script, "seq": SITE_SEQ, "tool": CODE_TOOL, "turn": TURN.to_string()}}),
    );
    writer.update(Kind::Other, None, Vec::new(), false);
}

fn entry_items(
    writer: &mut Writer<'_>,
    index: usize,
    shaped: &Shaped,
    unknown: &BTreeSet<u64>,
) -> Result<(), String> {
    match &shaped.entry {
        JournalEntry::Call {
            id, name, input, ..
        } => {
            writer.inputs.push((*id, name.clone(), input.clone()));
            let call = writer.call(*id, json!({"state": "running"}))?;
            writer.update(Kind::Entry(index), Some(shaped), vec![call], false);
            if external(name) {
                writer.other(
                    Kind::NestedCall,
                    json!({"call": {"call_id": nested_id(*id), "effect": "external", "input": input, "seq": SITE_SEQ, "tool": name, "turn": TURN.to_string()}}),
                );
            }
        }
        JournalEntry::Result {
            id,
            encoded,
            success,
        } => {
            let name = writer.call(*id, Value::Null)?["name"].clone();
            let outside = name.as_str().is_some_and(external);
            if outside && unknown.contains(id) {
                writer.other(
                    Kind::Other,
                    json!({"recovery": {"attempt": RECOVERY_ATTEMPT, "turn": TURN.to_string(), "unknown": [nested_id(*id)]}}),
                );
            }
            let call = writer.call(
                *id,
                json!({"encoded": encoded, "state": "completed", "success": success}),
            )?;
            writer.update(Kind::Entry(index), Some(shaped), vec![call], false);
            if outside && !unknown.contains(id) {
                let output = if *success {
                    json!({"ok": encoded})
                } else {
                    json!({"failed": encoded})
                };
                writer.other(
                    Kind::Other,
                    json!({"result": {"call_id": nested_id(*id), "output": output, "seq": SITE_SEQ}}),
                );
            }
        }
        _ => writer.update(Kind::Entry(index), Some(shaped), Vec::new(), false),
    }
    Ok(())
}

fn build(lived: &Lived, entries: &[Shaped], unknown: &BTreeSet<u64>) -> Result<Vec<Item>, String> {
    let mut writer = Writer {
        lived,
        items: Vec::with_capacity(entries.len() * 2 + 8),
        seq: 0,
        entries: 0,
        inputs: Vec::new(),
    };
    opening(&mut writer);
    for (index, shaped) in entries.iter().enumerate() {
        entry_items(&mut writer, index, shaped, unknown)?;
    }
    writer.update(Kind::Other, None, Vec::new(), true);
    let (output, outcome, result) = site_output(&lived.run.terminal, &lived.run.text)?;
    writer.other(
        Kind::SiteResult,
        json!({"result": {"call_id": origin(), "output": output, "seq": SITE_SEQ}}),
    );
    writer.other(
        Kind::Other,
        json!({"status": {"status": {"finished": {"last": SITE_SEQ, "outcome": outcome, "result": result}}, "turn": TURN.to_string()}}),
    );
    Ok(writer.items)
}

fn unknown_ids(entries: &[Shaped]) -> BTreeSet<u64> {
    entries
        .iter()
        .filter_map(|shaped| match &shaped.entry {
            JournalEntry::Result {
                id,
                encoded,
                success: false,
            } if *encoded == cells::unknown_outcome() => Some(*id),
            _ => None,
        })
        .collect()
}

fn write(directory: &Path, stem: &str, items: &[Item], checksum: bool) -> Result<(), String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("{}: cannot create: {error}", directory.display()))?;
    let mut text = String::new();
    for item in items {
        if checksum {
            let crc = CHECKSUM.checksum(item.body.as_bytes());
            text.push_str(&format!("{{\"crc\":{crc},\"record\":{}}}\n", item.body));
        } else {
            text.push_str(&item.body);
            text.push('\n');
        }
    }
    let path = directory.join(format!("{stem}.jsonl"));
    std::fs::write(&path, text)
        .map_err(|error| format!("{}: cannot write: {error}", path.display()))
}

struct Case {
    lived: Lived,
    entries: Vec<Shaped>,
    unknown: BTreeSet<u64>,
}

fn case(lived: Lived) -> Result<Case, String> {
    let entries = lived
        .entries
        .iter()
        .map(shaped)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{}: {error}", lived.name))?;
    let unknown = unknown_ids(&entries);
    Ok(Case {
        lived,
        entries,
        unknown,
    })
}

fn find<'a>(cases: &'a [Case], name: &str) -> &'a Case {
    cases
        .iter()
        .find(|case| case.lived.name == name)
        .expect("a broken variant names a golden case")
}

fn gap(cases: &[Case]) -> Result<Vec<Item>, String> {
    let case = find(cases, "sequential_classes");
    let mut items = build(&case.lived, &case.entries, &case.unknown)?;
    let middle = items
        .iter()
        .position(|item| item.kind == Kind::Entry(1))
        .expect("sequential_classes has a second entry");
    items.remove(middle);
    Ok(items)
}

fn nested_before_entry(cases: &[Case]) -> Result<Vec<Item>, String> {
    let case = find(cases, "sequential_classes");
    let mut items = build(&case.lived, &case.entries, &case.unknown)?;
    let call = items
        .iter()
        .position(|item| item.kind == Kind::NestedCall)
        .expect("sequential_classes calls an external tool");
    items.swap(call - 1, call);
    Ok(items)
}

fn unknown_as_failure(cases: &[Case]) -> Result<Vec<Item>, String> {
    let case = find(cases, "unknown_external");
    let entries = case
        .entries
        .iter()
        .map(|existing| match &existing.entry {
            JournalEntry::Result { id, .. } if case.unknown.contains(id) => {
                shaped(&JournalEntry::Result {
                    id: *id,
                    encoded: PLAIN_FAILURE.to_owned(),
                    success: false,
                })
            }
            other => shaped(other),
        })
        .collect::<Result<Vec<_>, _>>()?;
    build(&case.lived, &entries, &case.unknown)
}

fn site_text(cases: &[Case]) -> Result<Vec<Item>, String> {
    let case = find(cases, "parallel_out_of_order");
    let mut items = build(&case.lived, &case.entries, &case.unknown)?;
    let site = items
        .iter_mut()
        .find(|item| item.kind == Kind::SiteResult)
        .expect("every synthetic journal has its site result");
    site.body =
        json!({"result": {"call_id": origin(), "output": {"ok": TAMPERED_TEXT}, "seq": SITE_SEQ}})
            .to_string();
    Ok(items)
}

const BROKEN: [(&str, fn(&[Case]) -> Result<Vec<Item>, String>); 4] = [
    ("gap", gap),
    ("nested_call_before_its_entry", nested_before_entry),
    ("unknown_delivered_as_failure", unknown_as_failure),
    ("site_result_differs", site_text),
];

pub fn generate(directory: &Path) -> Result<usize, String> {
    let cases = golden::lived()?
        .into_iter()
        .map(case)
        .collect::<Result<Vec<_>, _>>()?;
    let mut written = 0;
    for case in &cases {
        let items = build(&case.lived, &case.entries, &case.unknown)
            .map_err(|error| format!("{}: {error}", case.lived.name))?;
        for (target, checksum) in TARGETS {
            let stem = format!("{}-{target}-{SEED}", case.lived.name);
            write(&directory.join("good"), &stem, &items, checksum)?;
            written += 1;
        }
    }
    let (target, checksum) = BROKEN_TARGET;
    for (name, broken) in BROKEN {
        let items = broken(&cases).map_err(|error| format!("{name}: {error}"))?;
        write(
            &directory.join(name),
            &format!("{name}-{target}-{SEED}"),
            &items,
            checksum,
        )?;
        written += 1;
    }
    Ok(written)
}
