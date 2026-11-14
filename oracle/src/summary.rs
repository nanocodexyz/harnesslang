use std::collections::BTreeMap;

use nanocode_turnlog::{EffectClass, Outcome};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub checksum: bool,
    pub harness: String,
    pub recovery: Recovery,
    pub records: usize,
    pub result: String,
    pub seed: u64,
    pub target: String,
    pub tools: BTreeMap<String, EffectClass>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    AtLoad,
    InStep,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Running,
    Finished,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnSummary {
    pub arguments: Value,
    pub attempt: u64,
    pub calls: BTreeMap<String, EffectClass>,
    pub handler: String,
    pub last: Option<u64>,
    pub open: BTreeMap<String, EffectClass>,
    pub outcome: Option<Outcome>,
    pub phase: Phase,
    pub replies: usize,
    pub result: Value,
    pub unknown: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointSummary {
    pub revision: u64,
    pub state: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellSummary {
    pub calls: BTreeMap<u64, String>,
    pub cell: u64,
    pub entries: Vec<Value>,
    pub finished: bool,
    pub origin: String,
    pub seq: u64,
    pub unknown: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CellLine {
    cell: CellSummary,
    record: usize,
}

const CELL_KEY: &str = "cell";

fn parse_line(row: &str) -> Result<Line, serde_json::Error> {
    let value: Value = serde_json::from_str(row)?;
    if value.get(CELL_KEY).is_none() {
        return serde_json::from_value(value);
    }
    let CellLine { cell, record } = serde_json::from_value(value)?;
    Ok(Line::Cell { cell, record })
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Line {
    Turn {
        record: usize,
        summary: TurnSummary,
        turn: String,
    },
    Checkpoint {
        checkpoint: CheckpointSummary,
        record: usize,
    },
    #[serde(skip)]
    Cell {
        cell: CellSummary,
        record: usize,
    },
    Rejected {
        error: String,
        record: usize,
    },
    Undecodable {
        decode_error: String,
    },
}

pub struct Summary {
    pub header: Header,
    pub lines: Vec<Line>,
}

pub fn parse(text: &str) -> Result<Summary, String> {
    let mut rows = text.lines();
    let first = rows.next().ok_or("the summary is empty")?;
    let header: Header =
        serde_json::from_str(first).map_err(|error| format!("summary header: {error}"))?;
    let mut lines = Vec::new();
    for (index, row) in rows.enumerate() {
        let line =
            parse_line(row).map_err(|error| format!("summary line {}: {error}", index + 2))?;
        lines.push(line);
    }
    Ok(Summary { header, lines })
}
