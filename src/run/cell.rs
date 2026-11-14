use std::collections::{BTreeMap, VecDeque};

use crate::effect::EffectClass;
use crate::ir::{Code, CodeId, Program, ToolId};
use crate::run::cell_journal::{
    CALL_LIMIT, CallState, CellLog, CellUpdate, ENTRY_BYTES, Entry, Header, JournaledCall,
    divergence_text, nested_call_id, unknown_outcome,
};
use crate::run::engine::{NestedRef, Stop, ToolRequest};
use crate::run::journal::{Output, Record, call_id};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::session::SiteLog;
use crate::run::value;
use crate::target::{self, Recovery};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Call { id: u64, name: String, input: Json },
    Timeout { id: u64, delay_ms: u64 },
    Clock,
    Monotonic,
    Random,
    Bytes { length: u64 },
    Idle,
    Done { output: String },
    Threw { error: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Issued,
    Refused {
        error: String,
    },
    Result {
        id: u64,
        encoded: String,
        success: bool,
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

pub fn step_json(step: &Step) -> Json {
    let (tag, body) = match step {
        Step::Call { id, name, input } => (
            "call",
            Json::object([
                ("id", Json::Number(*id)),
                ("input", input.clone()),
                ("name", Json::text(name)),
            ]),
        ),
        Step::Timeout { id, delay_ms } => (
            "timeout",
            Json::object([
                ("delay_ms", Json::Number(*delay_ms)),
                ("id", Json::Number(*id)),
            ]),
        ),
        Step::Clock => ("clock", Json::Null),
        Step::Monotonic => ("monotonic", Json::Null),
        Step::Random => ("random", Json::Null),
        Step::Bytes { length } => ("bytes", Json::object([("length", Json::Number(*length))])),
        Step::Idle => ("idle", Json::Null),
        Step::Done { output } => ("done", Json::object([("output", Json::text(output))])),
        Step::Threw { error } => ("threw", Json::object([("error", Json::text(error))])),
    };
    Json::object([(tag, body)])
}

pub fn input_json(input: &Input) -> Json {
    let (tag, body) = match input {
        Input::Issued => ("issued", Json::Null),
        Input::Refused { error } => ("refused", Json::object([("error", Json::text(error))])),
        Input::Result {
            id,
            encoded,
            success,
        } => (
            "result",
            Json::object([
                ("encoded", Json::text(encoded)),
                ("id", Json::Number(*id)),
                ("success", Json::Bool(*success)),
            ]),
        ),
        Input::Timer { id } => ("timer", Json::object([("id", Json::Number(*id))])),
        Input::Clock { value } => ("clock", Json::object([("value", Json::Number(*value))])),
        Input::Monotonic { value } => {
            ("monotonic", Json::object([("value", Json::Number(*value))]))
        }
        Input::Random { value } => ("random", Json::object([("value", Json::Fraction(*value))])),
        Input::Bytes { data } => (
            "bytes",
            Json::object([(
                "data",
                Json::Array(
                    data.iter()
                        .map(|byte| Json::Number(u64::from(*byte)))
                        .collect(),
                ),
            )]),
        ),
    };
    Json::object([(tag, body)])
}

pub fn event_json(event: &CellEvent) -> Json {
    match event {
        CellEvent::Delivered { cell, id } => Json::object([(
            "delivered",
            Json::object([("cell", Json::Number(*cell)), ("id", Json::Number(*id))]),
        )]),
        CellEvent::Diverged { cell, reason } => Json::object([(
            "diverged",
            Json::object([
                ("cell", Json::Number(*cell)),
                ("reason", Json::text(reason)),
            ]),
        )]),
        CellEvent::Yielded { cell } => {
            Json::object([("yielded", Json::object([("cell", Json::Number(*cell))]))])
        }
        CellEvent::Finished { cell, outcome } => Json::object([(
            "finished",
            Json::object([
                ("cell", Json::Number(*cell)),
                (
                    "output",
                    crate::run::journal::output_json(&outcome.output()),
                ),
            ]),
        )]),
    }
}

pub trait CellEngine {
    fn start(&mut self, cell: u64, script: &str) -> Step;
    fn resume(&mut self, cell: u64, input: Input) -> Step;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellPoint {
    BeforeHeader,
    AfterHeader,
    Entry,
    BeforeNested(EffectClass),
    AfterNested(EffectClass),
    Waiting,
    BeforeFinished,
    AfterFinished,
}

impl CellPoint {
    pub const fn name(self) -> &'static str {
        match self {
            Self::BeforeHeader => "cell_before_header",
            Self::AfterHeader => "cell_after_header",
            Self::Entry => "cell_entry",
            Self::BeforeNested(_) => "cell_before_nested",
            Self::AfterNested(_) => "cell_after_nested",
            Self::Waiting => "cell_waiting",
            Self::BeforeFinished => "cell_before_finished",
            Self::AfterFinished => "cell_after_finished",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    pub cell: u64,
    pub code: CodeId,
    pub timer: u64,
    pub delay_ms: u64,
    pub until_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellOutcome {
    Done(String),
    Threw(String),
    Unknown(String),
    Running(String),
}

pub const UNKNOWN_KEY: &str = "unknown";
pub const RUNNING_KEY: &str = "running";
pub const RUNNING_TEXT: &str = "Script running with cell ID ";
pub const TERMINATED_TEXT: &str = "Script terminated";
pub const NOT_FOUND_TEXT: &str = "exec cell ID not found";
pub const STOPPED_TEXT: &str = "Tool execution stopped by the user. The cell was terminated and no nested tool call was left running.";
pub const STOPPED_UNKNOWN_TEXT: &str = "Tool execution stopped by the user. The cell was terminated while a nested tool call was still running; that call's outcome is unknown. Do not assume it did not run or automatically repeat it.";

pub fn stopped_output(output: &Output) -> bool {
    matches!(output, Output::Ok(Json::String(text)) if text == STOPPED_TEXT || text == STOPPED_UNKNOWN_TEXT)
}

pub fn running_text(cell: u64) -> String {
    format!("{RUNNING_TEXT}{cell}")
}

impl CellOutcome {
    pub fn output(&self) -> Output {
        match self {
            Self::Done(text) => Output::Ok(Json::text(text)),
            Self::Threw(error) => Output::Failed(error.clone()),
            Self::Unknown(text) => Output::Ok(Json::object([(UNKNOWN_KEY, Json::text(text))])),
            Self::Running(text) => Output::Ok(Json::object([(RUNNING_KEY, Json::text(text))])),
        }
    }

    pub fn from_output(output: &Output) -> Result<Self, String> {
        match output {
            Output::Ok(Json::String(text)) => Ok(Self::Done(text.clone())),
            Output::Failed(error) => Ok(Self::Threw(error.clone())),
            Output::Ok(json) => {
                if let Some(text) = json.get(UNKNOWN_KEY).and_then(Json::as_str) {
                    return Ok(Self::Unknown(text.to_owned()));
                }
                match json.get(RUNNING_KEY).and_then(Json::as_str) {
                    Some(text) => Ok(Self::Running(text.to_owned())),
                    None => Err(format!(
                        "cell output {} is neither text, unknown nor running",
                        json.encode()
                    )),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellEvent {
    Delivered { cell: u64, id: u64 },
    Diverged { cell: u64, reason: String },
    Finished { cell: u64, outcome: CellOutcome },
    Yielded { cell: u64 },
}

pub trait CellHost {
    fn append(&mut self, record: Record);
    fn flush(&mut self) -> Result<(), Stop>;
    fn flush_withholding(&mut self, cell: u64) -> Result<(), Stop>;
    fn hook(&mut self, point: CellPoint) -> Result<(), Stop>;
    fn start(&mut self, cell: u64, script: &str) -> Step;
    fn resume(&mut self, cell: u64, input: Input) -> Step;
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output;
    fn attempt(&mut self, seq: u64, nested: u64, first: bool) -> Result<u64, Stop>;
    fn recovered(&mut self, seq: u64, nested: u64) -> Result<(), Stop>;
    fn clock(&mut self) -> u64;
    fn deadline(&mut self, site: u64, cell: u64) -> u64;
    fn random(&mut self) -> u64;
    fn wait(&mut self, wait: &Wait) -> Result<(), Stop>;
    fn waited(&mut self, wait: &Wait) -> bool;
    fn observe(&mut self, event: &CellEvent);
    fn cancelled(&mut self, code: CodeId, drain: bool) -> Result<bool, Stop>;
}

#[derive(Clone, Copy, Debug)]
pub enum Cell<'a> {
    Fresh(u64),
    Existing(&'a CellLog),
}

pub struct CellSite<'a> {
    pub program: &'a Program,
    pub code: CodeId,
    pub target: &'a target::Model,
    pub mutation: Mutation,
    pub session: &'a str,
    pub turn: u64,
    pub seq: u64,
    pub script: &'a str,
    pub cell: Cell<'a>,
    pub nested: &'a BTreeMap<(u64, u64), SiteLog>,
    pub header_buffered: bool,
    pub site: u64,
}

pub fn header_tools(program: &Program, code: &Code) -> Vec<Json> {
    code.tools
        .iter()
        .map(|tool| {
            let tool = &program.tools[*tool];
            let input = tool
                .params
                .iter()
                .map(|param| (param.name.clone(), Json::String(param.ty.name(program))))
                .collect();
            Json::object([
                ("input", Json::Object(input)),
                ("name", Json::text(&tool.name)),
            ])
        })
        .collect()
}

pub fn steps_max(code: &Code) -> u64 {
    2 * (code.journal_bytes.value / ENTRY_BYTES + 1) + 2
}

pub fn encode_output(output: &Output) -> (String, bool) {
    match output {
        Output::Ok(json) => (json.encode(), true),
        Output::Failed(error) => (Json::object([("error", Json::text(error))]).encode(), false),
    }
}

fn refusal(error: &str) -> (String, bool) {
    let json = Json::object([
        ("code", Json::text(CALL_LIMIT)),
        ("error", Json::text(error)),
        ("isError", Json::Bool(true)),
        ("retryable", Json::Bool(false)),
    ]);
    (json.encode(), false)
}

pub fn run(site: CellSite<'_>, host: &mut dyn CellHost) -> Result<CellOutcome, Stop> {
    if let Cell::Existing(log) = site.cell {
        assert!(!log.finished, "the engine never starts on a finished cell");
    }
    let mut runner = Runner::new(site, host);
    let driven = runner.drive();
    let outcome = match driven {
        Ok(outcome) => outcome,
        Err(Interrupt::Stop(stop)) => return Err(stop),
        Err(Interrupt::Stopped(unknown)) => return runner.finish(stopped(unknown)),
        Err(Interrupt::Yield) => {
            if runner.host.cancelled(runner.site.code, false)? {
                return runner.finish(stopped(false));
            }
            return Ok(runner.yielded());
        }
        Err(Interrupt::Threw(error)) => CellOutcome::Threw(error),
        Err(Interrupt::Diverged(reason)) => {
            CellOutcome::Unknown(divergence_text(runner.cell, &reason))
        }
        Err(Interrupt::Overflow) => {
            let reason = format!(
                "its journal passed journal_bytes ({} = {})",
                runner.code.journal_bytes.name, runner.code.journal_bytes.value
            );
            CellOutcome::Unknown(divergence_text(runner.cell, &reason))
        }
    };
    if runner.host.cancelled(runner.site.code, false)? {
        return runner.finish(stopped(false));
    }
    runner.finish(outcome)
}

fn stopped(unknown: bool) -> CellOutcome {
    let text = if unknown {
        STOPPED_UNKNOWN_TEXT
    } else {
        STOPPED_TEXT
    };
    CellOutcome::Done(text.to_owned())
}

enum Interrupt {
    Stop(Stop),
    Stopped(bool),
    Yield,
    Threw(String),
    Diverged(String),
    Overflow,
}

impl From<Stop> for Interrupt {
    fn from(stop: Stop) -> Self {
        Self::Stop(stop)
    }
}

type Ran<T> = Result<T, Interrupt>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Queued,
    Running,
    Completed,
}

#[derive(Clone, Debug)]
struct Nested {
    name: String,
    tool: Option<(ToolId, EffectClass)>,
    input: Json,
    phase: Phase,
    output: Option<(String, bool)>,
    refused: Option<String>,
    record: u64,
}

struct Runner<'a, 'h> {
    site: CellSite<'a>,
    code: &'a Code,
    host: &'h mut dyn CellHost,
    cell: u64,
    origin: String,
    script: String,
    consult: bool,
    replay: Vec<Entry>,
    cursor: usize,
    live: bool,
    entries: u64,
    fresh_entries: Vec<Entry>,
    changed: BTreeMap<u64, JournaledCall>,
    journaled: BTreeMap<u64, CallState>,
    next_seq: u64,
    header: Option<Header>,
    header_unsaved: bool,
    calls: BTreeMap<u64, Nested>,
    queue: VecDeque<u64>,
    issued: u64,
    timers: BTreeMap<u64, (u64, u64)>,
    armed: u64,
    bytes: u64,
    id_shift: u64,
    deadline: Option<u64>,
}

impl<'a, 'h> Runner<'a, 'h> {
    fn new(site: CellSite<'a>, host: &'h mut dyn CellHost) -> Self {
        let code = &site.program.codes[site.code];
        let origin = call_id(site.turn, site.seq);
        let ignores = site.mutation == Mutation::CellReplayIgnoresJournal;
        let (cell, replay, entries, next_seq, header, bytes, script) = match site.cell {
            Cell::Fresh(cell) => {
                let header = Header {
                    origin_call_id: origin.clone(),
                    script: site.script.to_owned(),
                    tools: header_tools(site.program, code),
                    yield_time_ms: code.yield_ms.value,
                };
                (
                    cell,
                    Vec::new(),
                    0,
                    0,
                    Some(header),
                    0,
                    site.script.to_owned(),
                )
            }
            Cell::Existing(log) => {
                let replay = if ignores {
                    Vec::new()
                } else {
                    log.entries.clone()
                };
                let entries = log.entries.len() as u64;
                let script = log.header.script.clone();
                (
                    log.cell,
                    replay,
                    entries,
                    log.next_seq,
                    None,
                    log.bytes(),
                    script,
                )
            }
        };
        let consult = matches!(site.cell, Cell::Existing(_)) && !ignores;
        let journaled = match site.cell {
            Cell::Existing(log) if !ignores => log
                .calls
                .iter()
                .map(|(id, call)| (*id, call.state.clone()))
                .collect(),
            Cell::Existing(_) | Cell::Fresh(_) => BTreeMap::new(),
        };
        Self {
            code,
            host,
            cell,
            origin,
            script,
            consult,
            live: replay.is_empty(),
            replay,
            cursor: 0,
            entries,
            fresh_entries: Vec::new(),
            changed: BTreeMap::new(),
            journaled,
            next_seq,
            header_unsaved: site.header_buffered,
            header,
            calls: BTreeMap::new(),
            queue: VecDeque::new(),
            issued: 0,
            timers: BTreeMap::new(),
            armed: 0,
            bytes,
            id_shift: 0,
            deadline: None,
            site,
        }
    }

    fn diverged(&mut self, reason: String) -> Interrupt {
        self.host.observe(&CellEvent::Diverged {
            cell: self.cell,
            reason: reason.clone(),
        });
        Interrupt::Diverged(reason)
    }

    fn unchecked(&self) -> bool {
        self.site.mutation == Mutation::CellLimitUnchecked
    }

    fn drive(&mut self) -> Ran<CellOutcome> {
        if self.header.is_some() {
            self.host.hook(CellPoint::BeforeHeader)?;
            self.emit(false);
            self.header_unsaved = true;
            self.host.hook(CellPoint::AfterHeader)?;
        }
        if self.site.script != self.script {
            return Err(self.diverged("its header holds another script than its call".to_owned()));
        }
        let script = self.script.clone();
        if self.live {
            self.arm_deadline();
        }
        let mut step = self.host.start(self.cell, &script);
        for _ in 0..steps_max(self.code) {
            let input = match step {
                Step::Done { output } => return self.ended(CellOutcome::Done(output)),
                Step::Threw { error } => return self.ended(CellOutcome::Threw(error)),
                Step::Call { id, name, input } => {
                    self.call(id, name, input)?;
                    Input::Issued
                }
                Step::Timeout { id, delay_ms } => self.timeout(id, delay_ms)?,
                Step::Clock => match self.record(Entry::Clock { value: 0 })? {
                    Entry::Clock { value } => Input::Clock { value },
                    other => panic!("a clock step records a clock entry, found {other:?}"),
                },
                Step::Monotonic => match self.record(Entry::Monotonic { value: 0 })? {
                    Entry::Monotonic { value } => Input::Monotonic { value },
                    other => panic!("a monotonic step records a monotonic entry, found {other:?}"),
                },
                Step::Random => match self.record(Entry::Random { value: 0 })? {
                    Entry::Random { value } => Input::Random { value },
                    other => panic!("a random step records a random entry, found {other:?}"),
                },
                Step::Bytes { length } => self.random_bytes(length)?,
                Step::Idle => self.input()?,
            };
            step = self.host.resume(self.cell, input);
        }
        Err(Interrupt::Overflow)
    }

    fn ended(&mut self, outcome: CellOutcome) -> Ran<CellOutcome> {
        if let Some(entry) = self.replay.get(self.cursor) {
            return Err(self.diverged(format!(
                "the script ended where its journal holds {}",
                entry.describe()
            )));
        }
        Ok(outcome)
    }

    fn random_bytes(&mut self, length: u64) -> Ran<Input> {
        if length > self.code.journal_bytes.value {
            return Err(Interrupt::Overflow);
        }
        let placeholder = Entry::Bytes {
            data: vec![0; usize::try_from(length).expect("a length within journal_bytes fits")],
        };
        match self.record(placeholder)? {
            Entry::Bytes { data } => Ok(Input::Bytes { data }),
            other => panic!("a bytes step records a bytes entry, found {other:?}"),
        }
    }

    fn fill(&mut self, step: Entry) -> Entry {
        match step {
            Entry::Timeout { id, delay_ms, .. } => Entry::Timeout {
                id,
                delay_ms,
                at_ms: self.host.clock(),
            },
            Entry::Clock { .. } => Entry::Clock {
                value: self.host.clock(),
            },
            Entry::Monotonic { .. } => Entry::Monotonic {
                value: self.host.clock(),
            },
            Entry::Random { .. } => Entry::Random {
                value: u32::try_from(self.host.random() >> 32).expect("the top 32 bits fit"),
            },
            Entry::Bytes { data } => {
                let mut filled = Vec::with_capacity(data.len());
                while filled.len() < data.len() {
                    filled.extend_from_slice(&self.host.random().to_le_bytes());
                }
                filled.truncate(data.len());
                Entry::Bytes { data: filled }
            }
            other => other,
        }
    }

    fn record(&mut self, step: Entry) -> Ran<Entry> {
        if let Some(recorded) = self.replay.get(self.cursor).cloned() {
            if recorded.is_input() || !recorded.same_step(&step) {
                return Err(self.diverged(format!(
                    "the script took {} where its journal holds {}",
                    step.describe(),
                    recorded.describe()
                )));
            }
            self.cursor += 1;
            if self.cursor == self.replay.len() {
                self.go_live()?;
            }
            return Ok(recorded);
        }
        self.go_live()?;
        let entry = self.fill(step);
        self.append_entry(entry.clone())?;
        Ok(entry)
    }

    fn append_entry(&mut self, entry: Entry) -> Ran<()> {
        assert!(self.live, "only a live cell appends entries");
        self.bytes += entry.bytes();
        if self.bytes > self.code.journal_bytes.value && !self.unchecked() {
            return Err(Interrupt::Overflow);
        }
        self.fresh_entries.push(entry);
        self.entries += 1;
        self.host.hook(CellPoint::Entry)?;
        Ok(())
    }

    fn emit(&mut self, finished: bool) {
        let header = self.header.take();
        if self.fresh_entries.is_empty() && self.changed.is_empty() && header.is_none() && !finished
        {
            return;
        }
        let entries = std::mem::take(&mut self.fresh_entries);
        let calls: Vec<JournaledCall> = std::mem::take(&mut self.changed).into_values().collect();
        for call in &calls {
            self.journaled.insert(call.id, call.state.clone());
        }
        let entries_from = self.entries - entries.len() as u64;
        let fresh = header.is_some();
        assert_eq!(
            fresh,
            self.next_seq == 0,
            "the header rides the first update only"
        );
        let update = CellUpdate {
            session_id: self.site.session.to_owned(),
            cell_id: self.cell,
            seq: self.next_seq,
            header,
            entries_from,
            entries,
            calls,
            finished,
        };
        self.next_seq += 1;
        self.host.append(Record::Cell {
            cell: self.cell,
            fresh,
            session: self.site.session.to_owned(),
            update,
        });
    }

    fn journaled(&self, id: u64) -> JournaledCall {
        let call = &self.calls[&id];
        let state = match call.phase {
            Phase::Queued => CallState::Queued,
            Phase::Running => CallState::Running,
            Phase::Completed => {
                let (encoded, success) = call
                    .output
                    .clone()
                    .expect("a completed call has its output");
                CallState::Completed { encoded, success }
            }
        };
        JournaledCall {
            id,
            call_id: nested_call_id(&self.origin, id),
            name: call.name.clone(),
            input: call.input.clone(),
            state,
        }
    }

    fn changed(&mut self, id: u64) {
        if self.live {
            let journaled = self.journaled(id);
            if self.journaled.get(&id) == Some(&journaled.state) {
                self.changed.remove(&id);
                return;
            }
            self.changed.insert(id, journaled);
        }
    }

    fn running(&self) -> u64 {
        self.calls
            .values()
            .filter(|call| call.phase == Phase::Running)
            .count() as u64
    }

    fn call(&mut self, id: u64, name: String, input: Json) -> Ran<()> {
        if self.calls.contains_key(&id) {
            return Err(self.diverged(format!("the engine issued nested call {id} twice")));
        }
        self.issued += 1;
        let tool = self
            .code
            .tools
            .iter()
            .find(|tool| self.site.program.tools[**tool].name == name)
            .map(|tool| (*tool, self.site.program.tools[*tool].effect));
        let refused = if tool.is_none() {
            Some(format!(
                "'{name}' is not a tool of code '{}'",
                self.code.name
            ))
        } else if self.issued > self.code.calls.value && !self.unchecked() {
            Some(format!(
                "the cell made its calls bound ({} = {}) of nested calls",
                self.code.calls.name, self.code.calls.value
            ))
        } else {
            None
        };
        let step = Entry::Call {
            id,
            name: name.clone(),
            input: input.clone(),
        };
        let recorded = self.record(step);
        let record = if self.live {
            id - self.id_shift.min(id - 1)
        } else {
            id
        };
        self.calls.insert(
            id,
            Nested {
                name,
                tool,
                input,
                phase: Phase::Queued,
                output: None,
                refused,
                record,
            },
        );
        if let Err(diverged) = recorded {
            if matches!(diverged, Interrupt::Diverged(_))
                && self.site.mutation == Mutation::DivergenceDispatches
                && self.calls[&id].refused.is_none()
            {
                self.calls.get_mut(&id).expect("inserted").phase = Phase::Running;
                self.run_tool(id)?;
            }
            return Err(diverged);
        }
        if self.running() < self.code.parallel.value || self.unchecked() {
            return self.start_call(id);
        }
        self.queue.push_back(id);
        self.changed(id);
        Ok(())
    }

    fn start_call(&mut self, id: u64) -> Ran<()> {
        self.calls
            .get_mut(&id)
            .expect("a started call was issued")
            .phase = Phase::Running;
        self.changed(id);
        if self.live {
            self.settle(id, true)?;
        }
        Ok(())
    }

    fn record_id(&self, id: u64) -> u64 {
        self.calls[&id].record
    }

    fn go_live(&mut self) -> Ran<()> {
        if self.live {
            return Ok(());
        }
        assert_eq!(
            self.cursor,
            self.replay.len(),
            "a cell goes live at its journal's end"
        );
        self.live = true;
        self.arm_deadline();
        if self.site.mutation == Mutation::NestedIdsRestart {
            self.id_shift = self.calls.keys().next_back().copied().unwrap_or(0);
        }
        let open: Vec<u64> = self
            .calls
            .iter()
            .filter(|(_, call)| call.phase == Phase::Running && call.output.is_none())
            .map(|(id, _)| *id)
            .collect();
        for id in open {
            self.settle(id, false)?;
        }
        Ok(())
    }

    fn set_output(&mut self, id: u64, output: (String, bool)) {
        let call = self.calls.get_mut(&id).expect("a settled call exists");
        assert!(call.output.is_none(), "a nested call settles once");
        call.output = Some(output);
    }

    fn unknown(&mut self, id: u64) {
        self.set_output(id, (unknown_outcome().encode(), false));
    }

    fn settle(&mut self, id: u64, started: bool) -> Ran<()> {
        let call = self.calls[&id].clone();
        if let Some(error) = &call.refused {
            self.set_output(id, refusal(error));
            return Ok(());
        }
        let (_, class) = call
            .tool
            .expect("a call that is not refused names a nested tool");
        let key = (self.site.seq, id);
        let logged = if self.consult {
            self.site.nested.get(&key).cloned()
        } else {
            None
        };
        match logged {
            Some(SiteLog::Done(_, output)) => self.set_output(id, encode_output(&output)),
            Some(SiteLog::Unknown(_)) => self.unknown(id),
            Some(SiteLog::Open(_)) => self.reopen(id)?,
            Some(SiteLog::Reply(..)) => panic!("a nested call never holds a model reply"),
            None if class == EffectClass::External => self.dispatch(id, true)?,
            None if !started && self.site.mutation == Mutation::NestedIdempotentReportedUnknown => {
                self.unknown(id)
            }
            None => {}
        }
        Ok(())
    }

    fn in_step(&self) -> bool {
        matches!(self.site.target.recovery, Recovery::InStep { .. })
    }

    fn reopen(&mut self, id: u64) -> Ran<()> {
        let rid = self.record_id(id);
        if self.in_step() {
            let attempt = self.host.attempt(self.site.seq, id, false)?;
            if attempt > 1 && self.site.mutation != Mutation::VercelNestedRetryRedispatches {
                self.host.recovered(self.site.seq, rid)?;
                self.header_unsaved = false;
                self.unknown(id);
                return Ok(());
            }
            return self.run_dispatch(id, EffectClass::External, true, false);
        }
        assert!(
            matches!(
                self.site.mutation,
                Mutation::CellRecoveryRedispatchesExternal | Mutation::RecoverySkipped
            ),
            "recovery turns every open nested external call unknown before the cell runs again"
        );
        self.run_dispatch(id, EffectClass::External, false, false)
    }

    fn dispatch(&mut self, id: u64, fresh: bool) -> Ran<()> {
        if self.host.cancelled(self.site.code, false)? {
            return Err(Interrupt::Stopped(false));
        }
        let call = self.calls[&id].clone();
        let (_, class) = call.tool.expect("a dispatched call names a nested tool");
        self.emit(false);
        if fresh && class == EffectClass::External {
            self.host.append(Record::NestedCall {
                turn: self.site.turn,
                seq: self.site.seq,
                nested: self.record_id(id),
                tool: call.name.clone(),
                effect: class,
                input: call.input.clone(),
            });
        }
        let step = self.in_step() && class == EffectClass::External;
        self.run_dispatch(id, class, step, step && fresh)
    }

    fn run_dispatch(&mut self, id: u64, class: EffectClass, step: bool, first: bool) -> Ran<()> {
        self.emit(false);
        let gate = self.site.target.gate_before_outbound;
        let required = class == EffectClass::External;
        let late = match self.site.mutation {
            Mutation::NestedFlushAfterDispatch => required && !gate,
            Mutation::CellGateReleasesEarly => gate,
            _ => false,
        };
        if (required || gate) && !late {
            if self.site.mutation == Mutation::CellHeaderRidesPastExternal && required {
                self.host.flush_withholding(self.cell)?;
            } else {
                self.host.flush()?;
                self.header_unsaved = false;
            }
        }
        if first {
            let attempt = self.host.attempt(self.site.seq, id, true)?;
            assert!(
                attempt == 1 || self.site.mutation == Mutation::CellReplayIgnoresJournal,
                "a fresh nested external step starts at its first attempt"
            );
        }
        let output = self.run_tool(id)?;
        if required && self.host.cancelled(self.site.code, true)? {
            self.host.recovered(self.site.seq, self.record_id(id))?;
            self.header_unsaved = false;
            return Err(Interrupt::Stopped(true));
        }
        if required {
            self.host.append(Record::NestedResult {
                turn: self.site.turn,
                seq: self.site.seq,
                nested: self.record_id(id),
                output: output.clone(),
            });
        }
        if step || late {
            self.host.flush()?;
            self.header_unsaved = false;
        }
        self.set_output(id, encode_output(&output));
        Ok(())
    }

    fn run_tool(&mut self, id: u64) -> Ran<Output> {
        let call = self.calls[&id].clone();
        let (tool, class) = call.tool.expect("a dispatched call names a nested tool");
        let returns = self.site.program.tools[tool].returns.clone();
        self.host.hook(CellPoint::BeforeNested(class))?;
        let output = self.host.tool(&ToolRequest {
            turn: self.site.turn,
            seq: self.site.seq,
            tool: &call.name,
            effect: class,
            input: &call.input,
            returns: &returns,
            admitted: None,
            job: false,
            nested: Some(NestedRef {
                id,
                record: self.record_id(id),
                cell: self.cell,
                code: self.site.code,
            }),
        });
        self.host.hook(CellPoint::AfterNested(class))?;
        let program = self.site.program;
        Ok(if self.site.mutation == Mutation::NestedEnumUnchecked {
            value::fallback_output(program, &returns, output)
        } else {
            value::checked_output(program, &call.name, &returns, output)
        })
    }

    fn timeout(&mut self, id: u64, delay_ms: u64) -> Ran<Input> {
        if !self.unchecked() {
            if self.armed >= self.code.timers.value {
                return Ok(Input::Refused {
                    error: format!(
                        "setTimeout: the cell armed its timers bound ({} = {})",
                        self.code.timers.name, self.code.timers.value
                    ),
                });
            }
            if delay_ms > self.code.timer_ms.value {
                return Ok(Input::Refused {
                    error: format!(
                        "setTimeout: {delay_ms} ms is past timer_ms ({} = {})",
                        self.code.timer_ms.name, self.code.timer_ms.value
                    ),
                });
            }
        }
        if self.timers.contains_key(&id) {
            return Err(self.diverged(format!("the engine armed timer {id} twice")));
        }
        let entry = self.record(Entry::Timeout {
            id,
            delay_ms,
            at_ms: 0,
        })?;
        let Entry::Timeout { at_ms, .. } = entry else {
            panic!("a timeout step records a timeout entry, found {entry:?}");
        };
        self.timers.insert(id, (delay_ms, at_ms));
        self.armed += 1;
        self.emit(false);
        Ok(Input::Issued)
    }

    fn input(&mut self) -> Ran<Input> {
        if let Some(entry) = self.replay.get(self.cursor).cloned() {
            return self.replayed_input(entry);
        }
        self.go_live()?;
        let ready = self
            .calls
            .iter()
            .find(|(_, call)| call.phase == Phase::Running)
            .map(|(id, _)| *id);
        if let Some(id) = ready {
            return self.deliver(id);
        }
        let earliest = self
            .timers
            .iter()
            .map(|(id, (delay, at))| (at.saturating_add(*delay), *id))
            .min();
        match earliest {
            Some((until, _)) if self.deadline.is_some_and(|deadline| until > deadline) => {
                Err(Interrupt::Yield)
            }
            Some((until, id)) => self.fire(id, until),
            None => Err(Interrupt::Threw(
                "the cell waits with no nested call running and no timer armed".to_owned(),
            )),
        }
    }

    fn replayed_input(&mut self, entry: Entry) -> Ran<Input> {
        match entry {
            Entry::Result {
                id,
                encoded,
                success,
            } => {
                let running = self
                    .calls
                    .get(&id)
                    .is_some_and(|call| call.phase == Phase::Running);
                if !running {
                    return Err(self.diverged(format!(
                        "its journal delivers nested call {id}, which the script has not started"
                    )));
                }
                self.cursor += 1;
                let call = self.calls.get_mut(&id).expect("running");
                call.output = Some((encoded.clone(), success));
                if self.cursor == self.replay.len() {
                    self.go_live()?;
                }
                self.complete(id)?;
                Ok(Input::Result {
                    id,
                    encoded,
                    success,
                })
            }
            Entry::Timer { id } => {
                if self.timers.remove(&id).is_none() {
                    return Err(self.diverged(format!(
                        "its journal fires timer {id}, which the script has not armed"
                    )));
                }
                self.cursor += 1;
                if self.cursor == self.replay.len() {
                    self.go_live()?;
                }
                Ok(Input::Timer { id })
            }
            other => Err(self.diverged(format!(
                "the script waits where its journal holds {}",
                other.describe()
            ))),
        }
    }

    fn complete(&mut self, id: u64) -> Ran<()> {
        self.calls
            .get_mut(&id)
            .expect("a delivered call exists")
            .phase = Phase::Completed;
        self.changed(id);
        self.host.observe(&CellEvent::Delivered {
            cell: self.cell,
            id,
        });
        while self.running() < self.code.parallel.value
            && let Some(next) = self.queue.pop_front()
        {
            self.start_call(next)?;
        }
        Ok(())
    }

    fn deliver(&mut self, id: u64) -> Ran<Input> {
        if self.calls[&id].output.is_none() {
            self.dispatch(id, false)?;
            if self.host.cancelled(self.site.code, true)? {
                return Err(Interrupt::Stopped(false));
            }
        }
        let (encoded, success) = self.calls[&id]
            .output
            .clone()
            .expect("a delivered call has its output");
        self.append_entry(Entry::Result {
            id,
            encoded: encoded.clone(),
            success,
        })?;
        self.complete(id)?;
        self.emit(false);
        Ok(Input::Result {
            id,
            encoded,
            success,
        })
    }

    fn fire(&mut self, id: u64, until_ms: u64) -> Ran<Input> {
        let (delay_ms, _) = self.timers[&id];
        let c2 = self.header_unsaved && self.site.mutation != Mutation::TimeoutRides;
        if c2 || self.site.target.wait_flushes() {
            self.emit(false);
            self.host.flush()?;
            self.header_unsaved = false;
        }
        let wait = Wait {
            cell: self.cell,
            code: self.site.code,
            timer: id,
            delay_ms,
            until_ms,
        };
        if !(self.site.target.wait_is_durable() && self.host.waited(&wait)) {
            self.host.hook(CellPoint::Waiting)?;
            self.host.wait(&wait)?;
        }
        if self.host.cancelled(self.site.code, true)? {
            return Err(Interrupt::Stopped(false));
        }
        self.timers.remove(&id);
        self.append_entry(Entry::Timer { id })?;
        self.emit(false);
        Ok(Input::Timer { id })
    }

    fn arm_deadline(&mut self) {
        if self.code.waits.is_none() {
            return;
        }
        let clock = self.host.deadline(self.site.site, self.cell);
        self.deadline = Some(clock.saturating_add(self.code.yield_ms.value));
    }

    fn yielded(mut self) -> CellOutcome {
        assert!(self.live, "a cell yields only while it runs live");
        assert!(
            self.running() == 0,
            "a cell yields only with no nested call running"
        );
        self.emit(false);
        self.host.observe(&CellEvent::Yielded { cell: self.cell });
        CellOutcome::Running(running_text(self.cell))
    }

    fn finish(mut self, outcome: CellOutcome) -> Result<CellOutcome, Stop> {
        self.host.hook(CellPoint::BeforeFinished)?;
        self.emit(true);
        self.host.hook(CellPoint::AfterFinished)?;
        self.host.observe(&CellEvent::Finished {
            cell: self.cell,
            outcome: outcome.clone(),
        });
        Ok(outcome)
    }
}
