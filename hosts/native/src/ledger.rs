use std::io::Write as _;
use std::path::{Path, PathBuf};

use harnesslang::run::journal::{Record, decode_lines};
use harnesslang::run::json::Json;

use crate::json::Value;

pub const LEDGER_FILE: &str = "usage.jsonl";
pub const USAGE_FILE: &str = "usage.json";
const JOURNAL_FILE: &str = "journal.jsonl";
const CHILDREN: &str = "children";
const JOURNAL_CHECKSUMS: bool = true;
const ATTEMPT_OK: &str = "ok";
const ATTEMPT_USAGE: usize = harnesslang::ir::ATTEMPT_USAGE;
const USAGE_FIELDS: usize = harnesslang::ir::USAGE_FIELDS.len();

pub struct Ledger {
    file: std::fs::File,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Ledger, String> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Ledger { file })
    }

    pub fn append(&mut self, line: Value) {
        let mut text = line.encode();
        text.push('\n');
        self.file
            .write_all(text.as_bytes())
            .expect("append to the usage ledger");
        self.file.sync_data().expect("sync the usage ledger");
    }

    pub fn mark(&mut self, kind: &str, at_ms: u64) {
        self.append(Value::object([
            ("kind", Value::text(kind)),
            ("at_ms", Value::number(at_ms)),
        ]));
    }
}

pub fn records(path: &Path) -> Result<Vec<Record>, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    decode_lines(&bytes, JOURNAL_CHECKSUMS)
        .map(|decoded| decoded.records)
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[derive(Default)]
pub struct Totals {
    pub tokens: [u64; USAGE_FIELDS],
    pub requests: u64,
    pub tool_calls: u64,
    pub wall_ms: u64,
    pub children: u64,
}

impl Totals {
    fn add_child(&mut self, child: &Totals) {
        for (sum, part) in self.tokens.iter_mut().zip(child.tokens) {
            *sum += part;
        }
        self.requests += child.requests;
        self.tool_calls += child.tool_calls;
        self.children += 1 + child.children;
    }
}

pub fn attempt_tokens(response: &Json) -> Option<[u64; USAGE_FIELDS]> {
    let Some(Json::Array(fields)) = response.get(ATTEMPT_OK) else {
        return None;
    };
    let usage = fields.get(ATTEMPT_USAGE..ATTEMPT_USAGE + USAGE_FIELDS)?;
    let mut tokens = [0; USAGE_FIELDS];
    for (slot, field) in tokens.iter_mut().zip(usage) {
        *slot = field.as_u64()?;
    }
    Some(tokens)
}

fn journal_totals(totals: &mut Totals, journal: &Path) -> Result<(), String> {
    if !journal.exists() {
        return Ok(());
    }
    for record in records(journal)? {
        if let Record::Model { response, .. } = record {
            totals.requests += 1;
            if let Some(tokens) = attempt_tokens(&response) {
                for (sum, part) in totals.tokens.iter_mut().zip(tokens) {
                    *sum += part;
                }
            }
        }
    }
    Ok(())
}

fn ledger_totals(totals: &mut Totals, ledger: &Path) -> Result<(), String> {
    let Ok(text) = std::fs::read_to_string(ledger) else {
        return Ok(());
    };
    let mut run: Option<(u64, u64)> = None;
    for line in text.lines().filter(|line| !line.is_empty()) {
        let Ok(value) = Value::decode(line) else {
            continue;
        };
        let at = value.get("at_ms").and_then(Value::as_u64).unwrap_or(0);
        match value.get("kind").and_then(Value::as_str) {
            Some("start") => {
                if let Some((start, last)) = run {
                    totals.wall_ms += last - start;
                }
                run = Some((at, at));
                continue;
            }
            Some("tool") => totals.tool_calls += 1,
            _ => {}
        }
        if let Some((start, last)) = run.as_mut() {
            *last = at.max(*last).max(*start);
        }
    }
    if let Some((start, last)) = run {
        totals.wall_ms += last - start;
    }
    Ok(())
}

pub fn totals(directory: &Path) -> Result<Totals, String> {
    let mut totals = Totals::default();
    journal_totals(&mut totals, &directory.join(JOURNAL_FILE))?;
    ledger_totals(&mut totals, &directory.join(LEDGER_FILE))?;
    for child in child_directories(directory)? {
        totals.add_child(&self::totals(&child)?);
    }
    Ok(totals)
}

pub fn child_directories(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let children = directory.join(CHILDREN);
    let Ok(entries) = std::fs::read_dir(&children) else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", children.display()))?;
        if entry.path().join(JOURNAL_FILE).exists() {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

pub fn write_usage(directory: &Path, model: &str, finish: &str) -> Result<(), String> {
    let totals = totals(directory)?;
    let [input, output, cache_read, cache_write] = totals.tokens;
    let usage = Value::object([
        ("model", Value::text(model)),
        ("input_tokens", Value::number(input)),
        ("output_tokens", Value::number(output)),
        ("cache_read_tokens", Value::number(cache_read)),
        ("cache_write_tokens", Value::number(cache_write)),
        ("requests", Value::number(totals.requests)),
        ("tool_calls", Value::number(totals.tool_calls)),
        ("wall_ms", Value::number(totals.wall_ms)),
        ("children", Value::number(totals.children)),
        ("finish", Value::text(finish)),
    ]);
    let temporary = directory.join(format!("{USAGE_FILE}.new"));
    std::fs::write(&temporary, usage.encode() + "\n")
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, directory.join(USAGE_FILE))
        .map_err(|error| format!("{USAGE_FILE}: {error}"))
}
