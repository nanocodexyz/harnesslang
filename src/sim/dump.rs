use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use crate::effect::EffectClass;
use crate::ir::Program;
use crate::run::cell_journal::{CallState, Entry};
use crate::run::journal::{self, Record, call_id, nested_id};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::session::{self, Call, Session, SiteLog, TurnLog};
use crate::sim::{self, SeedRun};
use crate::target::medium::Medium;
use crate::target::{self, Recovery, Target};

#[derive(Clone, Debug, Default)]
pub struct Dumped {
    pub journals: u64,
    pub records: u64,
    pub failures: Vec<String>,
}

pub fn checksummed(target: Target, mutation: Mutation) -> bool {
    match Medium::new(target, mutation) {
        Medium::Jsonl(file) => file.checksum,
        Medium::Rows(_) => false,
    }
}

pub fn dump(
    program: &Program,
    target: Target,
    mutation: Mutation,
    seeds: Range<u64>,
    directory: &Path,
    label: &str,
) -> Result<Dumped, String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("{}: cannot create: {error}", directory.display()))?;
    let checksum = checksummed(target, mutation);
    let mut dumped = Dumped::default();
    for seed in seeds {
        let run = sim::run_seed(program, target, mutation, seed);
        let (summary, records) = summary(program, target, checksum, seed, &run);
        let stem = format!("{label}-{}-{seed}", target.name());
        write(&directory.join(format!("{stem}.jsonl")), &run.journal)?;
        write(
            &directory.join(format!("{stem}.summary.jsonl")),
            summary.as_bytes(),
        )?;
        dumped.journals += 1;
        dumped.records += records;
        if let Err(violation) = &run.result {
            dumped.failures.push(format!(
                "seed {seed}: {}: {}",
                violation.invariant.name(),
                violation.detail
            ));
        }
    }
    Ok(dumped)
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes)
        .map_err(|error| format!("{}: cannot write: {error}", path.display()))
}

fn tools_json(program: &Program) -> Json {
    let mut tools = BTreeMap::new();
    for tool in &program.tools {
        assert!(
            !tools.contains_key(&tool.name),
            "the checker refuses a tool declared twice"
        );
        tools.insert(tool.name.clone(), Json::text(tool.effect.name()));
    }
    for code in &program.codes {
        let previous = tools.insert(code.name.clone(), Json::text(EffectClass::None.name()));
        assert!(
            previous.is_none(),
            "the checker refuses a code tool named like a tool"
        );
    }
    for listing in &program.listings {
        tools.insert(listing.name.clone(), Json::text(EffectClass::None.name()));
        for word in crate::sim::world::WORDS {
            let name = format!("{}{}{word}", listing.name, crate::ir::LISTED_TOOL_SEPARATOR);
            tools.insert(name, Json::text(listing.effect.name()));
        }
    }
    Json::Object(tools)
}

fn header(
    program: &Program,
    target: Target,
    checksum: bool,
    seed: u64,
    result: &str,
    records: u64,
) -> Json {
    let recovery = match target::model(target).recovery {
        Recovery::AtLoad => "at_load",
        Recovery::InStep { .. } => "in_step",
    };
    Json::object([
        ("checksum", Json::Bool(checksum)),
        ("harness", Json::text(&program.name)),
        ("recovery", Json::text(recovery)),
        ("records", Json::Number(records)),
        ("result", Json::text(result)),
        ("seed", Json::Number(seed)),
        ("target", Json::text(target.name())),
        ("tools", tools_json(program)),
    ])
}

pub fn run_result(run: &SeedRun) -> String {
    match &run.result {
        Ok(()) => "ok".to_owned(),
        Err(violation) => format!("{}: {}", violation.invariant.name(), violation.detail),
    }
}

pub fn summary(
    program: &Program,
    target: Target,
    checksum: bool,
    seed: u64,
    run: &SeedRun,
) -> (String, u64) {
    summarize(
        program,
        target,
        checksum,
        seed,
        &run_result(run),
        &run.journal,
    )
}

pub fn summarize(
    program: &Program,
    target: Target,
    checksum: bool,
    seed: u64,
    result: &str,
    bytes: &[u8],
) -> (String, u64) {
    let decoded = journal::decode_lines(bytes, checksum);
    let records = decoded
        .as_ref()
        .map_or(Vec::new(), |decoded| decoded.records.clone());
    let count = records.len() as u64;
    let mut lines = vec![header(program, target, checksum, seed, result, count)];
    if let Err(error) = &decoded {
        lines.push(Json::object([("decode_error", Json::text(error))]));
    }
    let mut session = Session::default();
    for (index, record) in records.iter().enumerate() {
        let position = Json::Number(index as u64);
        if let Err(error) = session::apply(&mut session, record) {
            lines.push(Json::object([
                ("error", Json::String(error)),
                ("record", position),
            ]));
            break;
        }
        lines.push(record_line(program, &session, record, position));
    }
    let mut text = String::new();
    for line in lines {
        text.push_str(&line.encode());
        text.push('\n');
    }
    (text, count)
}

fn record_line(program: &Program, session: &Session, record: &Record, position: Json) -> Json {
    if let Record::Cell { cell, .. } = record {
        return Json::object([("cell", cell_json(session, *cell)), ("record", position)]);
    }
    let Some(turn) = record.turn() else {
        let (revision, _) = session
            .checkpoints
            .last_key_value()
            .expect("a checkpoint record leaves a checkpoint");
        return Json::object([
            (
                "checkpoint",
                Json::object([
                    ("revision", Json::Number(*revision)),
                    ("state", Json::text(&program.name)),
                ]),
            ),
            ("record", position),
        ]);
    };
    let log = session
        .turns
        .get(&turn)
        .expect("a record that applied names an admitted turn");
    Json::object([
        ("record", position),
        ("summary", turn_json(turn, log)),
        ("turn", Json::String(turn.to_string())),
    ])
}

fn cell_json(session: &Session, cell: u64) -> Json {
    let (turn, seq) = session.cell_origins[&cell];
    let log = &session.turns[&turn].cells[&seq];
    assert_eq!(
        log.cell, cell,
        "a cell origin names the site holding that cell"
    );
    let calls = log
        .calls
        .values()
        .filter(|call| !log.delivered(call.id))
        .map(|call| {
            let state = match call.state {
                CallState::Queued => "queued",
                CallState::Running => "running",
                CallState::Completed { .. } => "completed",
            };
            (call.id.to_string(), Json::text(state))
        })
        .collect();
    Json::object([
        ("calls", Json::Object(calls)),
        ("cell", Json::Number(cell)),
        (
            "entries",
            Json::Array(log.entries.iter().map(Entry::to_json).collect()),
        ),
        ("finished", Json::Bool(log.finished)),
        ("origin", Json::String(call_id(turn, seq))),
        ("seq", Json::Number(log.next_seq)),
        (
            "unknown",
            Json::Array(log.unknown_calls().into_iter().map(Json::Number).collect()),
        ),
    ])
}

fn site_call(site: &SiteLog) -> Option<&Call> {
    match site {
        SiteLog::Open(call) | SiteLog::Done(call, _) | SiteLog::Unknown(call) => Some(call),
        SiteLog::Reply(_) => None,
    }
}

fn classes(turn: u64, log: &TurnLog, keep: impl Fn(&SiteLog) -> bool) -> Json {
    let mut calls = BTreeMap::new();
    for (seq, site) in &log.sites {
        if let Some(call) = site_call(site)
            && keep(site)
        {
            calls.insert(call_id(turn, *seq), Json::text(call.effect.name()));
        }
    }
    for ((seq, nested), site) in &log.nested {
        if let Some(call) = site_call(site)
            && keep(site)
        {
            calls.insert(
                nested_id(turn, *seq, *nested),
                Json::text(call.effect.name()),
            );
        }
    }
    Json::Object(calls)
}

fn turn_json(turn: u64, log: &TurnLog) -> Json {
    let mut unknown: Vec<Json> = log
        .open_seqs(EffectClass::may_be_unknown)
        .into_iter()
        .map(|seq| Json::String(call_id(turn, seq)))
        .collect();
    unknown.extend(
        log.open_nested(EffectClass::may_be_unknown)
            .into_iter()
            .map(|(seq, nested)| Json::String(nested_id(turn, seq, nested))),
    );
    let replies = log
        .sites
        .values()
        .filter(|site| matches!(site, SiteLog::Reply(_)))
        .count() as u64;
    let (phase, outcome, last, result) = match &log.finished {
        Some(finished) => (
            "finished",
            Json::text(finished.outcome.name()),
            Json::Number(finished.last),
            finished.result.clone(),
        ),
        None => ("running", Json::Null, Json::Null, Json::Null),
    };
    Json::object([
        ("arguments", log.arguments.clone()),
        ("attempt", Json::Number(log.attempts)),
        ("calls", classes(turn, log, |_| true)),
        ("handler", Json::text(&log.handler)),
        ("last", last),
        (
            "open",
            classes(turn, log, |site| matches!(site, SiteLog::Open(_))),
        ),
        ("outcome", outcome),
        ("phase", Json::text(phase)),
        ("replies", Json::Number(replies)),
        ("result", result),
        ("unknown", Json::Array(unknown)),
    ])
}
