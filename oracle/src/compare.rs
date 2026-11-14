use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use nanocode_turnlog::{EffectClass, Effects, Fold, Phase, Record, Status};
use serde_json::Value;

use crate::journal::{self, Lines};
use crate::summary::{self, CheckpointSummary, Header, Line, Recovery, TurnSummary};

const BUILTIN_NONE_TOOLS: [&str; 2] = ["now", "random"];

#[derive(Debug)]
pub struct Disagreement {
    pub journal: String,
    pub record: Option<usize>,
    pub what: String,
}

impl fmt::Display for Disagreement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.record {
            Some(record) => write!(formatter, "{} record {record}: {}", self.journal, self.what),
            None => write!(formatter, "{}: {}", self.journal, self.what),
        }
    }
}

#[derive(Debug, Default)]
pub struct Tally {
    pub journals: u64,
    pub records: u64,
    pub turns: u64,
    pub failed_turns: u64,
    pub calls: BTreeMap<&'static str, u64>,
    pub replies: u64,
    pub recoveries: u64,
    pub unknown: u64,
    pub open_external_prefixes: u64,
    pub rejected: u64,
    pub targets: BTreeMap<String, u64>,
}

impl fmt::Display for Tally {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let calls: Vec<String> = self
            .calls
            .iter()
            .map(|(class, count)| format!("{class}:{count}"))
            .collect();
        let targets: Vec<String> = self
            .targets
            .iter()
            .map(|(target, count)| format!("{target}:{count}"))
            .collect();
        write!(
            formatter,
            "journals={} records={} turns={} failed_turns={} calls={} replies={} recoveries={} unknown={} prefixes_with_open_external={} rejected_by_both={} targets={}",
            self.journals,
            self.records,
            self.turns,
            self.failed_turns,
            calls.join(","),
            self.replies,
            self.recoveries,
            self.unknown,
            self.open_external_prefixes,
            self.rejected,
            targets.join(",")
        )
    }
}

const fn class_name(class: EffectClass) -> &'static str {
    match class {
        EffectClass::External => "external",
        EffectClass::Idempotent => "idempotent",
        EffectClass::None => "none",
    }
}

fn record_turn(record: &Record) -> Option<String> {
    match record {
        Record::Admitted { turn, .. }
        | Record::Model { turn, .. }
        | Record::Recovery { turn, .. }
        | Record::Status { turn, .. } => Some(turn.clone()),
        Record::Call { call_id, .. } | Record::Result { call_id, .. } => {
            call_id.split_once('/').map(|(turn, _)| turn.to_owned())
        }
        _ => None,
    }
}

fn belongs(call_id: &str, turn: &str) -> bool {
    call_id
        .split_once('/')
        .is_some_and(|(owner, _)| owner == turn)
}

fn raw(text: &str) -> Value {
    serde_json::from_str(text).expect("a RawValue nanocode decoded is JSON")
}

struct Check<'a> {
    journal: &'a str,
    header: &'a Header,
    effects: Effects,
    fold: Fold,
    classes: BTreeMap<String, EffectClass>,
    tally: &'a mut Tally,
    found: &'a mut Vec<Disagreement>,
}

pub fn journal(
    stem: &str,
    bytes: &[u8],
    text: &str,
    tally: &mut Tally,
    found: &mut Vec<Disagreement>,
) {
    let mut disagree = |what: String| {
        found.push(Disagreement {
            journal: stem.to_owned(),
            record: None,
            what,
        });
    };
    let summary = match summary::parse(text) {
        Ok(summary) => summary,
        Err(error) => return disagree(format!("the interpreter's summary is unreadable: {error}")),
    };
    let ours_unreadable = summary.lines.iter().find_map(|line| match line {
        Line::Undecodable { decode_error } => Some(decode_error.clone()),
        _ => None,
    });
    let records = match (
        journal::records(bytes, summary.header.checksum),
        ours_unreadable,
    ) {
        (Lines::Records(records), None) => records,
        (Lines::Invalid(_), Some(_)) => {
            tally.rejected += 1;
            return;
        }
        (Lines::Invalid(error), None) => {
            return disagree(format!(
                "the oracle cannot read the journal ({error}); the interpreter can"
            ));
        }
        (Lines::Records(_), Some(error)) => {
            return disagree(format!(
                "the interpreter cannot read the journal ({error}); the oracle can"
            ));
        }
    };
    if records.len() != summary.header.records {
        return disagree(format!(
            "the line rules differ: the oracle reads {} records, the interpreter {}",
            records.len(),
            summary.header.records
        ));
    }
    let named = format!("-{}-{}", summary.header.target, summary.header.seed);
    if !stem.ends_with(&named) {
        return disagree(format!("the summary describes the journal of {named}"));
    }
    tally.journals += 1;
    *tally
        .targets
        .entry(summary.header.target.clone())
        .or_default() += 1;
    tally.records += records.len() as u64;
    let mut check = Check::new(stem, &summary.header, tally, found);
    for (index, body) in records.iter().enumerate() {
        if !check.step(index, body, summary.lines.get(index)) {
            return;
        }
    }
    check.end();
}

impl<'a> Check<'a> {
    fn new(
        journal: &'a str,
        header: &'a Header,
        tally: &'a mut Tally,
        found: &'a mut Vec<Disagreement>,
    ) -> Self {
        let mut effects = Effects::default();
        for tool in BUILTIN_NONE_TOOLS {
            effects.insert(tool, EffectClass::None);
        }
        for (tool, class) in &header.tools {
            effects.insert(tool.clone(), *class);
        }
        Self {
            journal,
            header,
            effects,
            fold: Fold::default(),
            classes: BTreeMap::new(),
            tally,
            found,
        }
    }

    fn disagree(&mut self, record: Option<usize>, what: String) {
        self.found.push(Disagreement {
            journal: self.journal.to_owned(),
            record,
            what,
        });
    }

    fn step(&mut self, index: usize, body: &str, line: Option<&Line>) -> bool {
        let record: Record = match serde_json::from_str(body) {
            Ok(record) => record,
            Err(error) => {
                self.disagree(Some(index), format!("nanocode cannot decode it: {error}"));
                return false;
            }
        };
        let Some(line) = line else {
            self.disagree(
                Some(index),
                "the interpreter's summary ends before it".to_owned(),
            );
            return false;
        };
        self.before(index, &record);
        let applied = self.fold.apply(&record, None);
        let ok = match (line, applied) {
            (Line::Rejected { record: at, .. }, Err(_)) if *at == index => {
                self.tally.rejected += 1;
                false
            }
            (Line::Rejected { error, .. }, Ok(())) => {
                self.disagree(
                    Some(index),
                    format!("the interpreter rejects it ({error}); nanocode folds it"),
                );
                false
            }
            (_, Err(error)) => {
                self.disagree(
                    Some(index),
                    format!("nanocode rejects it ({error}); the interpreter folds it"),
                );
                false
            }
            (
                Line::Turn {
                    record: at,
                    summary,
                    turn,
                },
                Ok(()),
            ) if *at == index => {
                self.turn(index, turn, summary, &record);
                true
            }
            (
                Line::Checkpoint {
                    checkpoint,
                    record: at,
                },
                Ok(()),
            ) if *at == index => {
                self.checkpoint(index, checkpoint);
                true
            }
            (other, Ok(())) => {
                self.disagree(
                    Some(index),
                    format!("the interpreter's summary line {other:?} does not describe it"),
                );
                false
            }
        };
        if !self.fold.unknown().is_empty() {
            self.tally.open_external_prefixes += 1;
        }
        ok
    }

    fn open_calls(&self, turn: &str) -> BTreeMap<String, EffectClass> {
        self.fold
            .open_calls()
            .filter(|(_, call)| call.turn.as_deref() == Some(turn))
            .map(|(id, call)| (id.to_owned(), call.effect))
            .collect()
    }

    fn open_external(&self, turn: &str) -> BTreeSet<String> {
        self.fold
            .unknown()
            .into_iter()
            .filter(|id| belongs(id, turn))
            .collect()
    }

    fn before(&mut self, index: usize, record: &Record) {
        match record {
            Record::Call {
                call_id,
                effect,
                tool,
                turn,
                ..
            } => {
                let expected = self.effects.of(tool);
                if *effect != expected {
                    self.disagree(Some(index), format!("call {call_id} to {tool} is recorded {effect:?}; nanocode's Effects over the program's declarations give {expected:?}"));
                }
                if !turn.as_deref().is_some_and(|turn| belongs(call_id, turn)) {
                    self.disagree(Some(index), format!("call {call_id} names turn {turn:?}"));
                }
                if self.classes.insert(call_id.clone(), *effect).is_some() {
                    self.disagree(Some(index), format!("call {call_id} is recorded twice"));
                }
                *self.tally.calls.entry(class_name(*effect)).or_default() += 1;
            }
            Record::Model { .. } => self.tally.replies += 1,
            Record::Recovery {
                attempt,
                turn,
                unknown,
                ..
            } => self.recovery(index, *attempt, turn, unknown),
            Record::Status { status, turn } => {
                let open = self.open_calls(turn);
                if !open.is_empty() {
                    self.disagree(
                        Some(index),
                        format!("turn {turn} finishes while nanocode holds calls open: {open:?}"),
                    );
                }
                if let Status::Finished { outcome, .. } = status {
                    if *outcome == nanocode_turnlog::Outcome::Failed {
                        self.tally.failed_turns += 1;
                    }
                } else {
                    self.disagree(
                        Some(index),
                        format!("turn {turn} has a status other than finished"),
                    );
                }
            }
            _ => {}
        }
    }

    fn recovery(&mut self, index: usize, attempt: u64, turn: &str, unknown: &[String]) {
        let next = self.fold.next_attempt(turn);
        if attempt != next {
            self.disagree(Some(index), format!("recovery of turn {turn} is attempt {attempt}; nanocode's next attempt is {next}"));
        }
        let open = self.open_external(turn);
        let listed: BTreeSet<String> = unknown.iter().cloned().collect();
        if listed.len() != unknown.len() {
            self.disagree(
                Some(index),
                format!("recovery of turn {turn} lists a call twice: {unknown:?}"),
            );
        }
        let agrees = match self.header.recovery {
            Recovery::AtLoad => listed == open,
            Recovery::InStep => listed.len() == 1 && listed.is_subset(&open),
        };
        if !agrees {
            self.disagree(Some(index), format!("recovery of turn {turn} reports {listed:?} unknown ({:?}); nanocode's fold holds open external calls {open:?}", self.header.recovery));
        }
        self.tally.recoveries += 1;
        self.tally.unknown += listed.len() as u64;
    }

    fn turn(&mut self, index: usize, turn: &str, ours: &TurnSummary, record: &Record) {
        if record_turn(record).as_deref() != Some(turn) {
            self.disagree(
                Some(index),
                format!(
                    "the interpreter summarises turn {turn} for a record of turn {:?}",
                    record_turn(record)
                ),
            );
            return;
        }
        let Some(found) = self.fold.turn(turn) else {
            self.disagree(Some(index), format!("nanocode's fold has no turn {turn}"));
            return;
        };
        let mut wrong = Vec::new();
        match &found.phase {
            Phase::Finished {
                last,
                outcome,
                result,
            } => {
                if ours.phase != summary::Phase::Finished {
                    wrong.push(format!(
                        "phase: nanocode finished, interpreter {:?}",
                        ours.phase
                    ));
                }
                if ours.outcome != Some(*outcome) || ours.last != Some(*last) {
                    wrong.push(format!(
                        "outcome: nanocode {outcome:?} at {last}, interpreter {:?} at {:?}",
                        ours.outcome, ours.last
                    ));
                }
                if raw(result.get()) != ours.result {
                    wrong.push(format!(
                        "result: nanocode {}, interpreter {}",
                        result.get(),
                        ours.result
                    ));
                }
            }
            Phase::Queued { .. } | Phase::Running { .. } => {
                if ours.phase != summary::Phase::Running || ours.outcome.is_some() {
                    wrong.push(format!(
                        "phase: nanocode running, interpreter {:?} {:?}",
                        ours.phase, ours.outcome
                    ));
                }
            }
        }
        if found.digest != ours.handler || raw(found.prompt.get()) != ours.arguments {
            wrong.push(format!(
                "admission: nanocode {} {}, interpreter {} {}",
                found.digest,
                found.prompt.get(),
                ours.handler,
                ours.arguments
            ));
        }
        let attempt = found.recovered.map_or(0, |recovered| recovered.attempt);
        if attempt != ours.attempt {
            wrong.push(format!(
                "attempt: nanocode {attempt}, interpreter {}",
                ours.attempt
            ));
        }
        let replies = self.fold.models(turn).len();
        if replies != ours.replies {
            wrong.push(format!(
                "model replies: nanocode {replies}, interpreter {}",
                ours.replies
            ));
        }
        wrong.extend(self.calls(turn, ours));
        for what in wrong {
            self.disagree(Some(index), format!("turn {turn}: {what}"));
        }
    }

    fn calls(&self, turn: &str, ours: &TurnSummary) -> Vec<String> {
        let mut wrong = Vec::new();
        let open = self.open_calls(turn);
        if open != ours.open {
            wrong.push(format!(
                "open calls: nanocode {open:?}, interpreter {:?}",
                ours.open
            ));
        }
        let unknown = self.open_external(turn);
        let reported: BTreeSet<String> = ours.unknown.iter().cloned().collect();
        if unknown != reported {
            wrong.push(format!(
                "calls recovery must report unknown: nanocode {unknown:?}, interpreter {reported:?}"
            ));
        }
        let classes: BTreeMap<String, EffectClass> = self
            .classes
            .iter()
            .filter(|(id, _)| belongs(id, turn))
            .map(|(id, class)| (id.clone(), *class))
            .collect();
        if classes != ours.calls {
            wrong.push(format!(
                "effect classes: nanocode {classes:?}, interpreter {:?}",
                ours.calls
            ));
        }
        wrong
    }

    fn checkpoint(&mut self, index: usize, ours: &CheckpointSummary) {
        let revision = self
            .fold
            .checkpoint(&ours.state)
            .map(|checkpoint| checkpoint.revision);
        if revision != Some(ours.revision) {
            self.disagree(
                Some(index),
                format!(
                    "checkpoint of {}: nanocode revision {revision:?}, interpreter {}",
                    ours.state, ours.revision
                ),
            );
        }
        if ours.state != self.header.harness {
            self.disagree(
                Some(index),
                format!(
                    "checkpoint state {} is not the harness {}",
                    ours.state, self.header.harness
                ),
            );
        }
    }

    fn end(&mut self) {
        self.tally.turns += self.fold.turns().len() as u64;
        if self.header.result != "ok" {
            return;
        }
        let unfinished: Vec<String> = self
            .fold
            .turns()
            .iter()
            .filter(|turn| !matches!(turn.phase, Phase::Finished { .. }))
            .map(|turn| turn.id.clone())
            .collect();
        if !unfinished.is_empty() {
            self.disagree(None, format!("the interpreter's run passed but nanocode's fold has unfinished turns {unfinished:?}"));
        }
        let open: Vec<String> = self
            .fold
            .open_calls()
            .map(|(id, _)| id.to_owned())
            .collect();
        if !open.is_empty() {
            self.disagree(
                None,
                format!(
                    "the interpreter's run passed but nanocode's fold holds calls open {open:?}"
                ),
            );
        }
    }
}
