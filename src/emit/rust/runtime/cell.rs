#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    pub name: &'static str,
    pub value: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NestedTool {
    pub name: &'static str,
    pub effect: EffectClass,
    pub returns: Returns,
    pub input: &'static [(&'static str, &'static str)],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeSpec {
    pub name: &'static str,
    pub tools: &'static [NestedTool],
    pub calls: Limit,
    pub parallel: Limit,
    pub timers: Limit,
    pub timer_ms: Limit,
    pub journal_bytes: Limit,
    pub yield_ms: Limit,
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NestedRef {
    pub id: u64,
    pub cell: u64,
    pub code: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    pub cell: u64,
    pub code: usize,
    pub timer: u64,
    pub delay_ms: u64,
    pub until_ms: u64,
}

pub trait CellEngine {
    fn start(&mut self, cell: u64, script: &str) -> Step;
    fn resume(&mut self, cell: u64, input: Input) -> Step;
    fn wait(&mut self, wait: &Wait) -> Result<(), Stop>;
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

pub const UNKNOWN_KEY: &str = "unknown";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellOutcome {
    Done(String),
    Threw(String),
    Unknown(String),
}

impl CellOutcome {
    pub fn output(&self) -> Output {
        match self {
            Self::Done(text) => Output::Ok(Json::text(text)),
            Self::Threw(error) => Output::Failed(error.clone()),
            Self::Unknown(text) => Output::Ok(Json::object([(UNKNOWN_KEY, Json::text(text))])),
        }
    }

    pub fn from_output(output: &Output) -> Result<Self, String> {
        match output {
            Output::Ok(Json::String(text)) => Ok(Self::Done(text.clone())),
            Output::Failed(error) => Ok(Self::Threw(error.clone())),
            Output::Ok(json) => match json.get(UNKNOWN_KEY).and_then(Json::as_str) {
                Some(text) => Ok(Self::Unknown(text.to_owned())),
                None => Err(format!(
                    "cell output {} is neither text nor unknown",
                    json.encode()
                )),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellEvent {
    Delivered { cell: u64, id: u64 },
    Diverged { cell: u64, reason: String },
    Finished { cell: u64, outcome: CellOutcome },
}

pub trait CellHost {
    fn append(&mut self, record: Record);
    fn flush(&mut self) -> Result<(), Stop>;
    fn hook(&mut self, point: CellPoint) -> Result<(), Stop>;
    fn start(&mut self, cell: u64, script: &str) -> Step;
    fn resume(&mut self, cell: u64, input: Input) -> Step;
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output;
    fn clock(&mut self) -> u64;
    fn random(&mut self) -> u64;
    fn wait(&mut self, wait: &Wait) -> Result<(), Stop>;
    fn observe(&mut self, event: &CellEvent);
}

#[derive(Clone, Copy, Debug)]
pub enum CellStart<'a> {
    Fresh(u64),
    Existing(&'a CellLog),
}

pub struct CellSite<'a> {
    pub code: usize,
    pub session: &'a str,
    pub turn: u64,
    pub seq: u64,
    pub script: &'a str,
    pub cell: CellStart<'a>,
    pub nested: &'a BTreeMap<(u64, u64), SiteLog>,
}

pub fn header_tools(code: &CodeSpec) -> Vec<Json> {
    code.tools
        .iter()
        .map(|tool| {
            let input = tool
                .input
                .iter()
                .map(|(name, ty)| ((*name).to_owned(), Json::text(ty)))
                .collect();
            Json::object([
                ("input", Json::Object(input)),
                ("name", Json::text(tool.name)),
            ])
        })
        .collect()
}

pub fn steps_max(code: &CodeSpec) -> u64 {
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

pub fn run_cell(site: CellSite<'_>, host: &mut dyn CellHost) -> Result<CellOutcome, Stop> {
    if let CellStart::Existing(log) = site.cell {
        assert!(!log.finished, "the engine never starts on a finished cell");
    }
    let mut runner = Runner::new(site, host);
    let outcome = match runner.drive() {
        Ok(outcome) => outcome,
        Err(Interrupt::Stop(stop)) => return Err(stop),
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
    runner.finish(outcome)
}

enum Interrupt {
    Stop(Stop),
    Threw(String),
    Diverged(String),
    Overflow,
}

impl From<Stop> for Interrupt {
    fn from(stop: Stop) -> Self {
        Self::Stop(stop)
    }
}

type Driven<T> = Result<T, Interrupt>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Queued,
    Running,
    Completed,
}

#[derive(Clone, Debug)]
struct Nested {
    name: String,
    tool: Option<NestedTool>,
    input: Json,
    phase: Phase,
    output: Option<(String, bool)>,
    refused: Option<String>,
}

struct Runner<'a, 'h> {
    site: CellSite<'a>,
    code: &'static CodeSpec,
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
}

impl<'a, 'h> Runner<'a, 'h> {
    fn new(site: CellSite<'a>, host: &'h mut dyn CellHost) -> Self {
        let code = &program::CODES[site.code];
        let origin = call_id(site.turn, site.seq);
        let (cell, replay, entries, next_seq, header, bytes, script) = match site.cell {
            CellStart::Fresh(cell) => {
                let header = Header {
                    origin_call_id: origin.clone(),
                    script: site.script.to_owned(),
                    tools: header_tools(code),
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
            CellStart::Existing(log) => (
                log.cell,
                log.entries.clone(),
                log.entries.len() as u64,
                log.next_seq,
                None,
                log.bytes(),
                log.header.script.clone(),
            ),
        };
        Self {
            consult: matches!(site.cell, CellStart::Existing(_)),
            code,
            host,
            cell,
            origin,
            script,
            live: replay.is_empty(),
            replay,
            cursor: 0,
            entries,
            fresh_entries: Vec::new(),
            changed: BTreeMap::new(),
            journaled: match site.cell {
                CellStart::Existing(log) => log
                    .calls
                    .iter()
                    .map(|(id, call)| (*id, call.state.clone()))
                    .collect(),
                CellStart::Fresh(_) => BTreeMap::new(),
            },
            next_seq,
            header_unsaved: false,
            header,
            calls: BTreeMap::new(),
            queue: VecDeque::new(),
            issued: 0,
            timers: BTreeMap::new(),
            armed: 0,
            bytes,
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

    fn drive(&mut self) -> Driven<CellOutcome> {
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

    fn ended(&mut self, outcome: CellOutcome) -> Driven<CellOutcome> {
        if let Some(entry) = self.replay.get(self.cursor) {
            let reason = format!(
                "the script ended where its journal holds {}",
                entry.describe()
            );
            return Err(self.diverged(reason));
        }
        Ok(outcome)
    }

    fn random_bytes(&mut self, length: u64) -> Driven<Input> {
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

    fn record(&mut self, step: Entry) -> Driven<Entry> {
        if let Some(recorded) = self.replay.get(self.cursor).cloned() {
            if recorded.is_input() || !recorded.same_step(&step) {
                let reason = format!(
                    "the script took {} where its journal holds {}",
                    step.describe(),
                    recorded.describe()
                );
                return Err(self.diverged(reason));
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

    fn append_entry(&mut self, entry: Entry) -> Driven<()> {
        assert!(self.live, "only a live cell appends entries");
        self.bytes += entry.bytes();
        if self.bytes > self.code.journal_bytes.value {
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

    fn call(&mut self, id: u64, name: String, input: Json) -> Driven<()> {
        if self.calls.contains_key(&id) {
            return Err(self.diverged(format!("the engine issued nested call {id} twice")));
        }
        self.issued += 1;
        let tool = self
            .code
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .copied();
        let refused = if tool.is_none() {
            Some(format!(
                "'{name}' is not a tool of code '{}'",
                self.code.name
            ))
        } else if self.issued > self.code.calls.value {
            Some(format!(
                "the cell made its calls bound ({} = {}) of nested calls",
                self.code.calls.name, self.code.calls.value
            ))
        } else {
            None
        };
        let recorded = self.record(Entry::Call {
            id,
            name: name.clone(),
            input: input.clone(),
        });
        self.calls.insert(
            id,
            Nested {
                name,
                tool,
                input,
                phase: Phase::Queued,
                output: None,
                refused,
            },
        );
        recorded?;
        if self.running() < self.code.parallel.value {
            return self.start_call(id);
        }
        self.queue.push_back(id);
        self.changed(id);
        Ok(())
    }

    fn start_call(&mut self, id: u64) -> Driven<()> {
        self.calls
            .get_mut(&id)
            .expect("a started call was issued")
            .phase = Phase::Running;
        self.changed(id);
        if self.live {
            self.settle(id)?;
        }
        Ok(())
    }

    fn go_live(&mut self) -> Driven<()> {
        if self.live {
            return Ok(());
        }
        assert_eq!(
            self.cursor,
            self.replay.len(),
            "a cell goes live at its journal's end"
        );
        self.live = true;
        let open: Vec<u64> = self
            .calls
            .iter()
            .filter(|(_, call)| call.phase == Phase::Running && call.output.is_none())
            .map(|(id, _)| *id)
            .collect();
        for id in open {
            self.settle(id)?;
        }
        Ok(())
    }

    fn set_output(&mut self, id: u64, output: (String, bool)) {
        let call = self.calls.get_mut(&id).expect("a settled call exists");
        assert!(call.output.is_none(), "a nested call settles once");
        call.output = Some(output);
    }

    fn settle(&mut self, id: u64) -> Driven<()> {
        let call = self.calls[&id].clone();
        if let Some(error) = &call.refused {
            self.set_output(id, refusal(error));
            return Ok(());
        }
        let tool = call
            .tool
            .expect("a call that is not refused names a nested tool");
        let logged = if self.consult {
            self.site.nested.get(&(self.site.seq, id)).cloned()
        } else {
            None
        };
        match logged {
            Some(SiteLog::Done(_, output)) => self.set_output(id, encode_output(&output)),
            Some(SiteLog::Unknown(_)) => self.set_output(id, (unknown_outcome().encode(), false)),
            Some(SiteLog::Open(_)) => panic!(
                "recovery turns every open nested external call unknown before the cell runs again"
            ),
            Some(SiteLog::Reply(_)) => panic!("a nested call never holds a model reply"),
            None if tool.effect == EffectClass::External => self.dispatch(id, true)?,
            None => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, id: u64, fresh: bool) -> Driven<()> {
        let call = self.calls[&id].clone();
        let tool = call.tool.expect("a dispatched call names a nested tool");
        let required = tool.effect == EffectClass::External;
        self.emit(false);
        if fresh && required {
            self.host.append(Record::NestedCall {
                turn: self.site.turn,
                seq: self.site.seq,
                nested: id,
                tool: call.name.clone(),
                effect: tool.effect,
                input: call.input.clone(),
            });
        }
        self.emit(false);
        if tool.effect.may_be_unknown() {
            self.host.flush()?;
            self.header_unsaved = false;
        }
        let output = self.run_tool(id, tool)?;
        if required {
            self.host.append(Record::NestedResult {
                turn: self.site.turn,
                seq: self.site.seq,
                nested: id,
                output: output.clone(),
            });
        }
        self.set_output(id, encode_output(&output));
        Ok(())
    }

    fn run_tool(&mut self, id: u64, tool: NestedTool) -> Driven<Output> {
        let input = self.calls[&id].input.clone();
        self.host.hook(CellPoint::BeforeNested(tool.effect))?;
        let output = self.host.tool(&ToolRequest {
            turn: self.site.turn,
            seq: self.site.seq,
            tool: tool.name,
            effect: tool.effect,
            input: &input,
            returns: tool.returns,
            admitted: None,
            nested: Some(NestedRef {
                id,
                cell: self.cell,
                code: self.site.code,
            }),
        });
        self.host.hook(CellPoint::AfterNested(tool.effect))?;
        Ok(checked_output(tool.name, tool.returns, output))
    }

    fn timeout(&mut self, id: u64, delay_ms: u64) -> Driven<Input> {
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

    fn input(&mut self) -> Driven<Input> {
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
            Some((until, id)) => self.fire(id, until),
            None => Err(Interrupt::Threw(
                "the cell waits with no nested call running and no timer armed".to_owned(),
            )),
        }
    }

    fn replayed_input(&mut self, entry: Entry) -> Driven<Input> {
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
                    let reason = format!(
                        "its journal delivers nested call {id}, which the script has not started"
                    );
                    return Err(self.diverged(reason));
                }
                self.cursor += 1;
                if self.cursor == self.replay.len() {
                    self.go_live()?;
                }
                let call = self.calls.get_mut(&id).expect("running");
                call.output = Some((encoded.clone(), success));
                self.complete(id)?;
                Ok(Input::Result {
                    id,
                    encoded,
                    success,
                })
            }
            Entry::Timer { id } => {
                if self.timers.remove(&id).is_none() {
                    let reason =
                        format!("its journal fires timer {id}, which the script has not armed");
                    return Err(self.diverged(reason));
                }
                self.cursor += 1;
                if self.cursor == self.replay.len() {
                    self.go_live()?;
                }
                Ok(Input::Timer { id })
            }
            other => {
                let reason = format!(
                    "the script waits where its journal holds {}",
                    other.describe()
                );
                Err(self.diverged(reason))
            }
        }
    }

    fn complete(&mut self, id: u64) -> Driven<()> {
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

    fn deliver(&mut self, id: u64) -> Driven<Input> {
        if self.calls[&id].output.is_none() {
            self.dispatch(id, false)?;
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

    fn fire(&mut self, id: u64, until_ms: u64) -> Driven<Input> {
        let (delay_ms, _) = self.timers[&id];
        if self.header_unsaved {
            self.emit(false);
            self.host.flush()?;
            self.header_unsaved = false;
        }
        self.host.hook(CellPoint::Waiting)?;
        self.host.wait(&Wait {
            cell: self.cell,
            code: self.site.code,
            timer: id,
            delay_ms,
            until_ms,
        })?;
        self.timers.remove(&id);
        self.append_entry(Entry::Timer { id })?;
        self.emit(false);
        Ok(Input::Timer { id })
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
