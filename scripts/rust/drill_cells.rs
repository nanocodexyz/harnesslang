use std::collections::{BTreeMap, BTreeSet};

use crate::harness::{
    self, CALL_INTERRUPTED, CallState, CellEngine, EffectClass, Entry, Input, Json, Record, Step,
    Stop, Wait, decode_checksummed_lines, nested_call_id, program,
};
use crate::Rng;
use crate::checks::{self, Line};

pub const CELL_REPLY_PPM: u64 = 200_000;
pub const OPS_MAX: u64 = 6;
pub const PARALLEL_MAX: u64 = 3;
pub const SLEEP_MS_MAX: u64 = 4;
pub const BYTES_MAX: u64 = 8;
pub const OP_CALLS_PPM: u64 = 550_000;
pub const OP_SLEEP_PPM: u64 = 200_000;
pub const END_THROW_PPM: u64 = 100_000;
const PPM: u64 = 1_000_000;
const CATCH: &str = ".catch((error) => error)";
const TOOLS_PREFIX: &str = "tools.";
const SLEEP_PREFIX: &str = "await new Promise((resolve) => setTimeout(resolve, ";
const SLEEP_SUFFIX: &str = "));";
const BYTES_PREFIX: &str = "crypto.getRandomValues(new Uint8Array(";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Calls(Vec<(String, Json)>),
    Sleep(u64),
    Clock,
    Monotonic,
    Random,
    Bytes(u64),
    Throw(String),
    Return(String),
}

fn call_text(name: &str, input: &Json) -> String {
    format!("{TOOLS_PREFIX}{name}({}){CATCH}", input.encode())
}

fn op_text(index: usize, op: &Op) -> String {
    match op {
        Op::Calls(calls) if calls.len() == 1 => {
            format!("const r{index} = await {};", call_text(&calls[0].0, &calls[0].1))
        }
        Op::Calls(calls) => {
            let items: Vec<String> = calls.iter().map(|(name, input)| call_text(name, input)).collect();
            format!("const p{index} = await Promise.all([{}]);", items.join(", "))
        }
        Op::Sleep(ms) => format!("{SLEEP_PREFIX}{ms}{SLEEP_SUFFIX}"),
        Op::Clock => format!("const c{index} = Date.now();"),
        Op::Monotonic => format!("const m{index} = performance.now();"),
        Op::Random => format!("const x{index} = Math.random();"),
        Op::Bytes(length) => format!("const b{index} = {BYTES_PREFIX}{length}));"),
        Op::Throw(text) => format!("throw new Error({});", Json::text(text).encode()),
        Op::Return(text) => format!("return {};", Json::text(text).encode()),
    }
}

fn parse_call(text: &str) -> Option<(String, Json)> {
    let rest = text.trim().strip_prefix(TOOLS_PREFIX)?;
    let (name, rest) = rest.split_once('(')?;
    let input = rest.strip_suffix(CATCH)?.strip_suffix(')')?;
    Some((name.to_owned(), Json::decode(input).ok()?))
}

fn parse_text(text: &str) -> Option<String> {
    Json::decode(text).ok()?.as_str().map(str::to_owned)
}

fn parse_line(line: &str) -> Option<Op> {
    if let Some(ms) = line.strip_prefix(SLEEP_PREFIX).and_then(|rest| rest.strip_suffix(SLEEP_SUFFIX)) {
        return ms.parse().ok().map(Op::Sleep);
    }
    if let Some(text) = line.strip_prefix("throw new Error(").and_then(|rest| rest.strip_suffix(");")) {
        return parse_text(text).map(Op::Throw);
    }
    if let Some(text) = line.strip_prefix("return ").and_then(|rest| rest.strip_suffix(';')) {
        return parse_text(text).map(Op::Return);
    }
    let value = line.strip_prefix("const ")?.split_once(" = ")?.1.strip_suffix(';')?;
    match value {
        "Date.now()" => return Some(Op::Clock),
        "performance.now()" => return Some(Op::Monotonic),
        "Math.random()" => return Some(Op::Random),
        _ => {}
    }
    if let Some(length) = value.strip_prefix(BYTES_PREFIX).and_then(|rest| rest.strip_suffix("))")) {
        return length.parse().ok().map(Op::Bytes);
    }
    if let Some(items) = value.strip_prefix("await Promise.all([").and_then(|rest| rest.strip_suffix("])")) {
        let mut calls = Vec::new();
        for piece in items.split_inclusive(CATCH) {
            calls.push(parse_call(piece.trim_start_matches(", "))?);
        }
        return (!calls.is_empty()).then_some(Op::Calls(calls));
    }
    parse_call(value.strip_prefix("await ")?).map(|call| Op::Calls(vec![call]))
}

pub fn script(rng: &mut Rng, code: usize, turn: u64, seq: u64) -> String {
    let spec = &program::CODES[code];
    let nested = |rng: &mut Rng, index: u64| {
        let tool = &spec.tools[rng.below(spec.tools.len() as u64) as usize];
        let input = tool
            .input
            .iter()
            .map(|(name, _)| (*name, Json::String(format!("{name}-{turn}-{seq}-{index}"))));
        (tool.name.to_owned(), Json::object(input))
    };
    let mut ops = Vec::new();
    for index in 0..1 + rng.below(OPS_MAX) {
        let roll = rng.below(PPM);
        ops.push(if roll < OP_CALLS_PPM {
            let size = 1 + rng.below(PARALLEL_MAX);
            Op::Calls((0..size).map(|call| nested(rng, index * PARALLEL_MAX + call)).collect())
        } else if roll < OP_CALLS_PPM + OP_SLEEP_PPM {
            Op::Sleep(1 + rng.below(SLEEP_MS_MAX))
        } else {
            match rng.below(4) {
                0 => Op::Clock,
                1 => Op::Monotonic,
                2 => Op::Random,
                _ => Op::Bytes(1 + rng.below(BYTES_MAX)),
            }
        });
    }
    let end = format!("cell-{turn}-{seq}");
    ops.push(if rng.chance(END_THROW_PPM) { Op::Throw(end) } else { Op::Return(end) });
    let lines: Vec<String> = ops.iter().enumerate().map(|(index, op)| op_text(index, op)).collect();
    lines.join("\n")
}

enum Phase {
    Next,
    Issuing { calls: Vec<(String, Json)>, issued: usize, pending: BTreeSet<u64> },
    Awaiting { pending: BTreeSet<u64> },
    Sleeping { timer: u64 },
    Value,
    Ended,
}

struct ScriptCell {
    ops: Vec<Op>,
    index: usize,
    phase: Phase,
    next_call: u64,
    next_timer: u64,
}

impl ScriptCell {
    fn next(&mut self) -> Step {
        let Some(op) = self.ops.get(self.index).cloned() else {
            self.phase = Phase::Ended;
            return Step::Done { output: String::new() };
        };
        self.index += 1;
        match op {
            Op::Calls(calls) => {
                self.phase = Phase::Issuing { calls, issued: 0, pending: BTreeSet::new() };
                self.issue()
            }
            Op::Sleep(delay_ms) => {
                self.next_timer += 1;
                self.phase = Phase::Sleeping { timer: self.next_timer };
                Step::Timeout { id: self.next_timer, delay_ms }
            }
            Op::Clock => self.value(Step::Clock),
            Op::Monotonic => self.value(Step::Monotonic),
            Op::Random => self.value(Step::Random),
            Op::Bytes(length) => self.value(Step::Bytes { length }),
            Op::Throw(error) => {
                self.phase = Phase::Ended;
                Step::Threw { error }
            }
            Op::Return(output) => {
                self.phase = Phase::Ended;
                Step::Done { output }
            }
        }
    }

    fn value(&mut self, step: Step) -> Step {
        self.phase = Phase::Value;
        step
    }

    fn issue(&mut self) -> Step {
        let Phase::Issuing { calls, issued, pending } = &mut self.phase else {
            panic!("the scripted engine issues calls only while issuing");
        };
        if let Some((name, input)) = calls.get(*issued).cloned() {
            *issued += 1;
            self.next_call += 1;
            pending.insert(self.next_call);
            return Step::Call { id: self.next_call, name, input };
        }
        let pending = std::mem::take(pending);
        self.phase = Phase::Awaiting { pending };
        Step::Idle
    }

    fn resume(&mut self, input: Input) -> Step {
        match (&mut self.phase, input) {
            (Phase::Issuing { .. }, Input::Issued) => self.issue(),
            (Phase::Awaiting { pending }, Input::Result { id, .. }) => {
                assert!(pending.remove(&id), "the runner delivers only calls the script awaits");
                if pending.is_empty() { self.next() } else { Step::Idle }
            }
            (Phase::Sleeping { .. }, Input::Issued) => Step::Idle,
            (Phase::Sleeping { .. }, Input::Refused { error }) => {
                self.phase = Phase::Ended;
                Step::Threw { error }
            }
            (Phase::Sleeping { timer }, Input::Timer { id }) => {
                assert_eq!(*timer, id, "the runner fires the timer the script armed");
                self.next()
            }
            (Phase::Value, Input::Clock { .. } | Input::Monotonic { .. } | Input::Random { .. } | Input::Bytes { .. }) => {
                self.next()
            }
            (_, input) => panic!("the scripted engine got {input:?} where it did not wait for it"),
        }
    }
}

pub struct ScriptEngine {
    pub journal: String,
    cells: BTreeMap<u64, ScriptCell>,
    pub waits: u64,
}

impl ScriptEngine {
    pub fn new(journal: &str) -> Self {
        Self { journal: journal.to_owned(), cells: BTreeMap::new(), waits: 0 }
    }
}

impl CellEngine for ScriptEngine {
    fn start(&mut self, cell: u64, script: &str) -> Step {
        let ops: Option<Vec<Op>> = script.lines().map(parse_line).collect();
        let Some(ops) = ops else {
            return Step::Threw { error: "SyntaxError: the scripted engine cannot run this script".to_owned() };
        };
        let mut state = ScriptCell { ops, index: 0, phase: Phase::Next, next_call: 0, next_timer: 0 };
        let step = state.next();
        self.cells.insert(cell, state);
        step
    }

    fn resume(&mut self, cell: u64, input: Input) -> Step {
        self.cells.get_mut(&cell).expect("the runner resumes a started cell").resume(input)
    }

    fn wait(&mut self, wait: &Wait) -> Result<(), Stop> {
        let durable = durable_records(&self.journal);
        let header = durable.iter().any(|record| matches!(record, Record::Cell { cell, fresh: true, .. } if *cell == wait.cell));
        assert!(header, "header_before_wait: cell {} waits on timer {} before its header is durable", wait.cell, wait.timer);
        self.waits += 1;
        std::thread::sleep(std::time::Duration::from_millis(wait.delay_ms));
        Ok(())
    }
}

pub fn durable_records(journal: &str) -> Vec<Record> {
    let bytes = std::fs::read(journal).unwrap_or_default();
    decode_checksummed_lines(&bytes).records
}

pub fn check_c1(journal: &str, turn: u64, seq: u64, nested: u64, tool: &str) {
    let durable = durable_records(journal);
    let origin = harness::call_id(turn, seq);
    let header = durable.iter().any(|record| match record {
        Record::Cell { update, .. } => update.header.as_ref().is_some_and(|header| header.origin_call_id == origin),
        _ => false,
    });
    let call = durable.iter().any(|record| matches!(record,
        Record::NestedCall { turn: t, seq: s, nested: n, tool: name, .. } if (*t, *s, *n) == (turn, seq, nested) && name == tool));
    let id = nested_call_id(&origin, nested);
    assert!(header, "nested_external_once: {id} ran before its cell's header was durable");
    assert!(call, "nested_external_once: {id} ran before its call record was durable");
}

#[derive(Default)]
pub struct CellCounts {
    pub cells: u64,
    pub external: u64,
    pub unknown: u64,
    pub other: u64,
    pub threw: u64,
}

fn nested_class(name: &str) -> Option<EffectClass> {
    program::CODES.iter().flat_map(|code| code.tools.iter()).find(|tool| tool.name == name).map(|tool| tool.effect)
}

fn unknown_entry(log: &harness::CellLog, id: u64) -> Option<bool> {
    log.entries.iter().find_map(|entry| match entry {
        Entry::Result { id: delivered, encoded, success } if *delivered == id => {
            Some(!success && encoded.contains(CALL_INTERRUPTED))
        }
        _ => None,
    })
}

pub fn check_cells(session: &harness::Session, records: &[Record], lines: &[Line]) -> CellCounts {
    let mut reported: BTreeSet<String> = BTreeSet::new();
    let mut recorded: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for record in records {
        match record {
            Record::Recovery { turn, nested, .. } => {
                for (seq, id) in nested {
                    assert!(reported.insert(harness::nested_id(*turn, *seq, *id)), "a nested call is reported unknown once");
                }
            }
            Record::NestedCall { turn, seq, nested, tool, effect, .. } => {
                assert_eq!(*effect, EffectClass::External, "only nested external calls write call records");
                let id = harness::nested_id(*turn, *seq, *nested);
                assert!(recorded.insert(id, (tool.clone(), false)).is_none(), "seq_unique: a nested call is recorded once");
            }
            Record::NestedResult { turn, seq, nested, .. } => {
                let id = harness::nested_id(*turn, *seq, *nested);
                recorded.get_mut(&id).expect("a nested result follows its call").1 = true;
            }
            _ => {}
        }
    }
    let mut dispatched: BTreeMap<&str, u64> = BTreeMap::new();
    for dispatch in checks::dispatches(lines).filter(|dispatch| dispatch.external && dispatch.nested()) {
        *dispatched.entry(&dispatch.id).or_default() += 1;
        let logged = recorded.get(&dispatch.id).map(|(tool, _)| tool.as_str());
        assert_eq!(logged, Some(dispatch.tool.as_str()), "nested_external_once: {} ran without its durable call record", dispatch.id);
    }
    for (id, count) in &dispatched {
        assert_eq!(*count, 1, "nested_external_once: {id} dispatched {count} times");
    }
    let mut counts = CellCounts::default();
    for (id, (_, done)) in &recorded {
        let unknown = reported.contains(id);
        assert!(*done ^ unknown, "unknown_nested_reported: {id} has a result {done} and is reported unknown {unknown}");
        if *done {
            assert_eq!(dispatched.get(id.as_str()), Some(&1), "{id} has a result without a dispatch");
        }
    }
    for (turn, log) in &session.turns {
        for (seq, cell) in &log.cells {
            counts.cells += 1;
            assert!(cell.finished, "cell_finishes_once: cell {} of {turn}/{seq} finished", cell.cell);
            let site = log.sites.get(seq);
            assert!(matches!(site, Some(harness::SiteLog::Done(..))), "cell_finishes_once: the site {turn}/{seq} of cell {} has its result", cell.cell);
            if let Some(harness::SiteLog::Done(_, harness::Output::Failed(_))) = site {
                counts.threw += 1;
            }
            for call in cell.calls.values() {
                let id = nested_call_id(&harness::call_id(*turn, *seq), call.id);
                let Some(class) = nested_class(&call.name) else { continue };
                let unknown = unknown_entry(cell, call.id);
                assert!(matches!(call.state, CallState::Completed { .. }), "cell_finishes_once: {id} completed before its cell finished");
                if class == EffectClass::External {
                    counts.external += 1;
                    let listed = reported.contains(&id);
                    assert_eq!(unknown, Some(listed), "unknown_nested_reported: {id} is delivered unknown exactly when a recovery lists it");
                    counts.unknown += u64::from(listed);
                } else {
                    counts.other += 1;
                    assert_eq!(unknown, Some(false), "nested_known: {id} ends with a known result");
                }
            }
        }
    }
    counts
}
