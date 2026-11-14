use std::collections::{BTreeMap, BTreeSet};

use crate::effect::EffectClass;
use crate::ir::{
    self, BinaryOp, Expr, Handler, Kept, Parallel, Program, Site, SiteClass, SiteKind, Stmt, Type,
};
use crate::run::blob::Blobs;
use crate::run::cell::{self, CellEvent, CellHost, CellOutcome, CellPoint, Input, Step, Wait};
use crate::run::cell_journal::{CellLog, divergence_text};
use crate::run::journal::{Delivery, Inbound, Outcome, Output, Record, call_id};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::session::{Call, Recorded, SiteLog, TurnLog};
use crate::run::value::{self, Value};
use crate::target::inbound::{self, Drain};
use crate::target::{self, Recovery};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Crash,
    Fenced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    Stop(Stop),
    Corrupt(String),
}

enum Halt {
    Exit(Exit),
    Trap(String),
    Suspend(Suspension),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suspension {
    pub turn: u64,
    pub seq: u64,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resume {
    pub key: String,
    pub answer: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    Finished(Finish),
    Suspended(Suspension),
}

impl From<Stop> for Halt {
    fn from(stop: Stop) -> Self {
        Self::Exit(Exit::Stop(stop))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    BeforeFlush,
    AfterFlush,
    BeforeRun(SiteKind, EffectClass),
    AfterRun(SiteKind, EffectClass),
    BeforeAcknowledge,
    Cell(CellPoint),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finish {
    pub turn: u64,
    pub outcome: Outcome,
    pub result: Json,
}

pub struct ToolRequest<'a> {
    pub turn: u64,
    pub seq: u64,
    pub tool: &'a str,
    pub effect: EffectClass,
    pub input: &'a Json,
    pub returns: &'a Type,
    pub nested: Option<NestedRef>,
    pub admitted: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NestedRef {
    pub id: u64,
    pub record: u64,
    pub cell: u64,
    pub code: crate::ir::CodeId,
}

pub struct ListingRequest<'a> {
    pub turn: u64,
    pub seq: u64,
    pub listing: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Recorded,
    Duplicate,
    Full,
    Undeclared,
    Finished,
}

impl Answer {
    pub const ALL: [Self; 5] = [
        Self::Recorded,
        Self::Duplicate,
        Self::Full,
        Self::Undeclared,
        Self::Finished,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Duplicate => "duplicate",
            Self::Full => "full",
            Self::Undeclared => "undeclared",
            Self::Finished => "finished",
        }
    }

    pub const fn delivered(self) -> bool {
        matches!(self, Self::Recorded | Self::Duplicate)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub turn: u64,
    pub position: u64,
    pub frozen: bool,
    pub kind: &'static str,
    pub visible: Vec<String>,
}

pub struct ModelRequest<'a> {
    pub turn: u64,
    pub seq: u64,
    pub calls_max: u64,
    pub mode: Option<&'a str>,
    pub context: &'a Value,
    pub blobs: &'a Blobs,
}

pub trait World {
    fn hook(&mut self, point: Point) -> Result<(), Stop>;
    fn flush(&mut self, epoch: u64, records: &[Record]) -> Result<(), Stop>;
    fn attempt(&mut self, turn: u64, seq: u64) -> Result<u64, Stop>;
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output;
    fn listing(&mut self, request: &ListingRequest<'_>) -> Output;
    fn tools(
        &mut self,
        requests: &[ToolRequest<'_>],
        limit: Option<u64>,
    ) -> Result<Vec<(usize, Output)>, Stop> {
        let order: Vec<usize> = (0..requests.len()).collect();
        run_batch(self, requests, limit, &order)
    }
    fn model(&mut self, request: &ModelRequest<'_>) -> Json;
    fn now(&mut self) -> u64;
    fn random(&mut self) -> u64;
    fn acknowledge(&mut self, finish: &Finish);
    fn cell_start(&mut self, cell: u64, script: &str) -> Step;
    fn cell_resume(&mut self, cell: u64, input: Input) -> Step;
    fn nested_attempt(
        &mut self,
        turn: u64,
        seq: u64,
        nested: u64,
        first: bool,
    ) -> Result<u64, Stop>;
    fn wait(&mut self, wait: &Wait) -> Result<(), Stop>;
    fn waited(&mut self, wait: &Wait) -> bool;
    fn observe(&mut self, event: &CellEvent);
    fn clock(&mut self) -> u64;
    fn sleep(&mut self, turn: u64, seq: u64, until_ms: u64) -> Result<u64, Stop>;
    fn resume(&mut self, suspension: &Suspension) -> Option<Resume>;
    fn resumed(&mut self, suspension: &Suspension, resume: &Resume, matched: bool);
    fn inbox(&mut self, turn: u64) -> Vec<Delivery>;
    fn answer(&mut self, turn: u64, position: u64, answers: &[(String, Answer)]);
    fn read(&mut self, read: &Read);
}

pub fn close_inbox(world: &mut dyn World, turn: u64, position: u64, events: &[Recorded]) {
    let arrived = world.inbox(turn);
    if arrived.is_empty() {
        return;
    }
    let answers: Vec<(String, Answer)> = arrived
        .into_iter()
        .map(|delivery| {
            let recorded = events.iter().any(|event| event.delivery.id == delivery.id);
            let answer = if recorded {
                Answer::Duplicate
            } else {
                Answer::Finished
            };
            (delivery.id, answer)
        })
        .collect();
    world.answer(turn, position, &answers);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Batch {
    Start(usize),
    Complete(usize),
}

pub fn batch_width(count: usize, limit: Option<u64>) -> usize {
    match limit {
        Some(limit) => {
            assert!(limit > 0, "a concurrency limit admits at least one request");
            usize::try_from(limit).map_or(count, |limit| limit.min(count))
        }
        None => count,
    }
}

pub fn schedule(count: usize, limit: Option<u64>, order: &[usize]) -> Vec<Batch> {
    assert_eq!(order.len(), count, "a batch completes each of its requests");
    let width = batch_width(count, limit);
    let mut events = Vec::with_capacity(2 * count);
    let mut started = 0;
    let mut done = vec![false; count];
    while started < width {
        events.push(Batch::Start(started));
        started += 1;
    }
    for &index in order {
        assert!(index < started, "a request completes only after it started");
        assert!(!done[index], "a request completes once");
        done[index] = true;
        events.push(Batch::Complete(index));
        if started < count {
            events.push(Batch::Start(started));
            started += 1;
        }
    }
    assert_eq!(started, count, "every request of a batch starts");
    events
}

pub fn run_batch<W: World + ?Sized>(
    world: &mut W,
    requests: &[ToolRequest<'_>],
    limit: Option<u64>,
    order: &[usize],
) -> Result<Vec<(usize, Output)>, Stop> {
    let mut outputs: Vec<Option<Output>> = vec![None; requests.len()];
    let mut completed = Vec::with_capacity(requests.len());
    for event in schedule(requests.len(), limit, order) {
        match event {
            Batch::Start(index) => {
                let effect = requests[index].effect;
                world.hook(Point::BeforeRun(SiteKind::Dispatch, effect))?;
                outputs[index] = Some(world.tool(&requests[index]));
            }
            Batch::Complete(index) => {
                let effect = requests[index].effect;
                world.hook(Point::AfterRun(SiteKind::Dispatch, effect))?;
                let output = outputs[index]
                    .take()
                    .expect("a started request has an output");
                completed.push((index, output));
            }
        }
    }
    Ok(completed)
}

struct Pending {
    index: usize,
    seq: u64,
    call: Call,
    reopened: bool,
}

#[derive(Clone, Copy)]
pub struct Setup<'a> {
    pub program: &'a Program,
    pub target: &'a target::Model,
    pub mutation: Mutation,
}

pub enum Mode<'a> {
    Live {
        world: &'a mut dyn World,
        epoch: u64,
    },
    Replay,
}

pub struct Engine<'a> {
    program: &'a Program,
    handler: &'a Handler,
    target: &'a target::Model,
    mutation: Mutation,
    mode: Mode<'a>,
    turn: u64,
    state: Vec<Value>,
    locals: Vec<Option<Value>>,
    sites: BTreeMap<u64, SiteLog>,
    seq: u64,
    attempts: u64,
    buffer: Vec<Record>,
    acknowledged: bool,
    cells: BTreeMap<u64, CellLog>,
    nested: BTreeMap<(u64, u64), SiteLog>,
    next_cell: u64,
    events: Vec<Recorded>,
    taken: usize,
    frozen: Option<u64>,
    finishing: bool,
    retired: u64,
    unretired: BTreeMap<usize, Vec<Value>>,
    admitted: BTreeMap<ir::ListingId, BTreeMap<String, String>>,
    listed: BTreeMap<ir::ListingId, BTreeMap<String, String>>,
    notices: BTreeMap<ir::ListingId, String>,
    discovered: BTreeMap<ir::ListingId, BTreeSet<String>>,
    blobs: Blobs,
}

enum Flow {
    Next,
    Return(Value),
}

pub struct Ran {
    pub end: End,
    pub state: Vec<Value>,
    pub retired: u64,
}

impl<'a> Engine<'a> {
    pub fn new(
        setup: Setup<'a>,
        handler: &'a Handler,
        mode: Mode<'a>,
        turn: u64,
        state: Vec<Value>,
        log: &TurnLog,
    ) -> Self {
        let Setup {
            program,
            target,
            mutation,
        } = setup;
        assert_eq!(
            log.handler, handler.name,
            "a turn runs the handler it was admitted for"
        );
        Self {
            program,
            handler,
            target,
            mutation,
            mode,
            turn,
            state,
            locals: vec![None; handler.locals.len()],
            sites: log.sites.clone(),
            seq: 0,
            attempts: log.attempts,
            buffer: Vec::new(),
            acknowledged: false,
            cells: log.cells.clone(),
            nested: log.nested.clone(),
            next_cell: 0,
            events: log.events.clone(),
            taken: 0,
            frozen: frozen(log),
            finishing: false,
            retired: 0,
            unretired: BTreeMap::new(),
            admitted: BTreeMap::new(),
            listed: BTreeMap::new(),
            notices: BTreeMap::new(),
            discovered: BTreeMap::new(),
            blobs: Blobs::default(),
        }
    }

    pub fn with_blobs(mut self, blobs: Blobs) -> Self {
        self.blobs = blobs;
        self
    }

    pub fn with_next_cell(mut self, next_cell: u64) -> Self {
        self.next_cell = next_cell;
        self
    }

    pub fn append(&mut self, record: Record) {
        if matches!(self.mode, Mode::Live { .. }) {
            self.buffer.push(record);
        }
    }

    pub fn next_attempt(&mut self) -> u64 {
        self.attempts += 1;
        self.attempts
    }

    pub fn run(mut self, arguments: &[Value]) -> Result<Ran, Exit> {
        assert_eq!(
            arguments.len(),
            self.handler.params.len(),
            "a turn has one argument per parameter"
        );
        for (local, argument) in self.handler.params.iter().zip(arguments) {
            self.locals[*local] = Some(argument.clone());
        }
        let body = &self.handler.body;
        let flow = match self.admit() {
            Ok(()) => self.block(body),
            Err(halt) => Err(halt),
        };
        let end = match flow {
            Ok(Flow::Return(value)) => self
                .finish(
                    Outcome::Completed,
                    value::to_json(self.program, &self.handler.returns, &value),
                )
                .map(End::Finished),
            Ok(Flow::Next) => panic!("the checker proves every handler returns"),
            Err(Halt::Trap(trap)) => self
                .finish(
                    Outcome::Failed,
                    Json::object([("trap", Json::String(trap))]),
                )
                .map(End::Finished),
            Err(Halt::Suspend(suspension)) => Ok(End::Suspended(suspension)),
            Err(Halt::Exit(exit)) => Err(exit),
        }?;
        Ok(Ran {
            end,
            state: self.state,
            retired: self.retired,
        })
    }

    pub fn resume(&mut self, suspension: &Suspension, resume: &Resume) -> Result<bool, Exit> {
        let Some(SiteLog::Open(call)) = self.sites.get(&suspension.seq).cloned() else {
            panic!("a resume reaches a turn whose suspend is open");
        };
        let matched = resume.key == suspension.key || self.mutation == Mutation::ResumeUnmatched;
        if matched {
            let output = Output::Ok(Json::String(resume.answer.clone()));
            self.append(Record::Result {
                turn: self.turn,
                seq: suspension.seq,
                output: output.clone(),
            });
            self.sites
                .insert(suspension.seq, SiteLog::Done(call, output));
            self.flush().map_err(halt_exit)?;
            assert!(
                self.buffer.is_empty(),
                "a resume's result is durable before it is answered"
            );
        }
        if let Mode::Live { world, .. } = &mut self.mode {
            world.resumed(suspension, resume, matched);
        }
        Ok(matched)
    }

    fn admit(&mut self) -> Result<(), Halt> {
        let Some(requires) = &self.handler.requires else {
            return Ok(());
        };
        if self.eval(requires)?.as_bool() {
            return Ok(());
        }
        Err(Halt::Trap(format!("requires({})", self.handler.name)))
    }

    fn finish(&mut self, outcome: Outcome, result: Json) -> Result<Finish, Exit> {
        let finish = Finish {
            turn: self.turn,
            outcome,
            result: result.clone(),
        };
        self.append(Record::Status {
            turn: self.turn,
            last: self.seq,
            outcome,
            result,
        });
        if self.mutation != Mutation::SealSkipped || self.retired == 0 {
            let payload = self.checkpoint_payload();
            self.append(Record::Checkpoint {
                revision: self.turn,
                state: self.program.name.clone(),
                payload,
            });
        }
        if self.mutation == Mutation::FinishBeforeFlush {
            self.acknowledge(&finish);
        }
        self.finishing = true;
        self.flush().map_err(halt_exit)?;
        assert!(
            self.buffer.is_empty(),
            "a turn's status is durable before it is acknowledged"
        );
        self.hook(Point::BeforeAcknowledge).map_err(halt_exit)?;
        self.acknowledge(&finish);
        if self.program.events.any()
            && let Mode::Live { world, .. } = &mut self.mode
        {
            close_inbox(&mut **world, self.turn, self.seq, &self.events);
        }
        Ok(finish)
    }

    fn checkpoint_payload(&self) -> String {
        if self.mutation == Mutation::CheckpointHashOrder {
            return value::state_text_hashed(self.program, &self.state);
        }
        let mut state = self.state.clone();
        for (field, unretired) in &self.unretired {
            let Value::List(items) = &mut state[*field] else {
                panic!("only a list field retires entries");
            };
            items.splice(0..0, unretired.iter().cloned());
        }
        value::state_json(self.program, &state).encode()
    }

    fn acknowledge(&mut self, finish: &Finish) {
        if self.acknowledged {
            return;
        }
        self.acknowledged = true;
        if let Mode::Live { world, .. } = &mut self.mode {
            world.acknowledge(finish);
        }
    }

    fn hook(&mut self, point: Point) -> Result<(), Halt> {
        match &mut self.mode {
            Mode::Live { world, .. } => world.hook(point).map_err(Halt::from),
            Mode::Replay => Ok(()),
        }
    }

    fn flush(&mut self) -> Result<(), Halt> {
        if !self.write()? {
            return Ok(());
        }
        if !self.finishing {
            self.drain_at(Drain::AfterFlush)?;
        }
        Ok(())
    }

    fn write(&mut self) -> Result<bool, Halt> {
        let Mode::Live { world, epoch } = &mut self.mode else {
            return Ok(false);
        };
        if self.buffer.is_empty() {
            return Ok(false);
        }
        world.hook(Point::BeforeFlush)?;
        world.flush(*epoch, &self.buffer)?;
        self.buffer.clear();
        world.hook(Point::AfterFlush)?;
        Ok(true)
    }

    fn drain_at(&mut self, point: Drain) -> Result<(), Halt> {
        if inbound::of(self.target.target).drain != point {
            return Ok(());
        }
        self.drain()
    }

    fn drain(&mut self) -> Result<(), Halt> {
        if !self.program.events.any() {
            return Ok(());
        }
        let Mode::Live { world, .. } = &mut self.mode else {
            return Ok(());
        };
        let arrived = world.inbox(self.turn);
        if arrived.is_empty() {
            return Ok(());
        }
        let position = self.seq;
        let mut answers = Vec::with_capacity(arrived.len());
        for delivery in arrived {
            let answer = self.admit_event(position, &delivery);
            answers.push((delivery.id, answer));
        }
        let recorded = answers
            .iter()
            .any(|(_, answer)| *answer == Answer::Recorded);
        let early = self.mutation == Mutation::EventAckEarly;
        if early {
            self.answer(position, &answers);
        }
        if recorded {
            self.write_events()?;
        }
        if !early {
            self.answer(position, &answers);
        }
        Ok(())
    }

    fn answer(&mut self, position: u64, answers: &[(String, Answer)]) {
        if let Mode::Live { world, .. } = &mut self.mode {
            world.answer(self.turn, position, answers);
        }
    }

    fn write_events(&mut self) -> Result<(), Halt> {
        if inbound::of(self.target.target).drain == Drain::AfterRun {
            self.write()?;
            return Ok(());
        }
        assert!(
            self.buffer
                .iter()
                .all(|record| matches!(record, Record::Event { .. })),
            "a step boundary leaves only event records to append"
        );
        let Mode::Live { world, epoch } = &mut self.mode else {
            unreachable!("only a live turn records events");
        };
        world.flush(*epoch, &self.buffer)?;
        self.buffer.clear();
        Ok(())
    }

    fn admit_event(&mut self, position: u64, delivery: &Delivery) -> Answer {
        let redelivered = self.mutation == Mutation::EventRedelivered;
        if !redelivered
            && self
                .events
                .iter()
                .any(|event| event.delivery.id == delivery.id)
        {
            return Answer::Duplicate;
        }
        let declared = &self.program.events;
        match &delivery.inbound {
            Inbound::Cancel if !declared.cancel => return Answer::Undeclared,
            Inbound::Cancel => {
                let cancelled = self
                    .events
                    .iter()
                    .any(|event| event.delivery.inbound == Inbound::Cancel);
                if cancelled {
                    return Answer::Duplicate;
                }
            }
            Inbound::Steer(_) => {
                let Some(bound) = &declared.steers else {
                    return Answer::Undeclared;
                };
                if steer_count(&self.events) >= bound.value {
                    return Answer::Full;
                }
            }
        }
        let Some(seq) = event_seq(
            declared.records_max(),
            self.turn,
            self.events.len() as u64 + 1,
        ) else {
            return Answer::Full;
        };
        self.append(Record::Event {
            turn: self.turn,
            seq,
            at: position,
            delivery: delivery.clone(),
        });
        self.events.push(Recorded {
            at: position,
            delivery: delivery.clone(),
        });
        Answer::Recorded
    }

    fn visible(&self, kind: &Inbound) -> Vec<&Recorded> {
        let unstamped = self.mutation == Mutation::EventReadUnstamped;
        self.events
            .iter()
            .filter(|event| {
                std::mem::discriminant(&event.delivery.inbound) == std::mem::discriminant(kind)
            })
            .filter(|event| unstamped || event.at <= self.seq)
            .collect()
    }

    fn observe(&mut self, kind: &Inbound, visible: Vec<String>) {
        let frozen = self.frozen.is_some_and(|frozen| self.seq <= frozen);
        if let Mode::Live { world, .. } = &mut self.mode {
            world.read(&Read {
                turn: self.turn,
                position: self.seq,
                frozen,
                kind: kind.name(),
                visible,
            });
        }
    }

    fn cancelled(&mut self) -> bool {
        let ids: Vec<String> = self
            .visible(&Inbound::Cancel)
            .iter()
            .map(|event| event.delivery.id.clone())
            .collect();
        let cancelled = !ids.is_empty();
        self.observe(&Inbound::Cancel, ids);
        cancelled
    }

    fn take(&mut self) -> Value {
        let kind = Inbound::Steer(String::new());
        let visible: Vec<(String, String)> = self
            .visible(&kind)
            .iter()
            .map(|event| match &event.delivery.inbound {
                Inbound::Steer(message) => (event.delivery.id.clone(), message.clone()),
                Inbound::Cancel => unreachable!("visible keeps one kind"),
            })
            .collect();
        assert!(
            self.taken <= visible.len(),
            "the steers a turn sees only grow"
        );
        let fresh: Vec<Value> = visible[self.taken..]
            .iter()
            .map(|(_, message)| Value::Text(message.clone()))
            .collect();
        self.taken = visible.len();
        self.observe(&kind, visible.into_iter().map(|(id, _)| id).collect());
        Value::List(fresh)
    }

    fn block(&mut self, statements: &[Stmt]) -> Result<Flow, Halt> {
        for statement in statements {
            if let Flow::Return(value) = self.statement(statement)? {
                return Ok(Flow::Return(value));
            }
        }
        Ok(Flow::Next)
    }

    fn statement(&mut self, statement: &Stmt) -> Result<Flow, Halt> {
        match statement {
            Stmt::Let { local, value } => {
                self.locals[*local] = Some(self.eval(value)?);
            }
            Stmt::Assign { field, value } => {
                self.state[*field] = self.eval(value)?;
            }
            Stmt::Push { field, value } => self.push(*field, value)?,
            Stmt::Shrink { field, kept } => self.shrink(*field, kept)?,
            Stmt::KeepBlobs { field, count } => {
                let attached = self
                    .program
                    .blobs
                    .as_ref()
                    .expect("the checker keeps blobs only in a program with blobs")
                    .attached;
                let evict = self.mutation != Mutation::BlobsUnevicted;
                let Value::List(items) = &mut self.state[*field] else {
                    panic!("keep blobs targets a list field");
                };
                crate::run::blob::keep_blobs(items, attached, count.value, evict);
            }
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                let branch = if self.eval(condition)?.as_bool() {
                    then
                } else {
                    otherwise
                };
                return self.block(branch);
            }
            Stmt::Match { subject, arms, .. } => {
                let subject = self.eval(subject)?;
                let (variant, fields) = subject.as_variant();
                let arm = &arms[variant];
                assert_eq!(arm.variant, variant, "arms are in variant order");
                for (binding, field) in arm.bindings.iter().zip(fields) {
                    if let Some(local) = binding {
                        self.locals[*local] = Some(field.clone());
                    }
                }
                return self.block(&arm.body);
            }
            Stmt::Loop { index, bound, body } => {
                let iterations = if self.mutation == Mutation::LoopPastBound {
                    bound.value + 1
                } else {
                    bound.value
                };
                for iteration in 0..iterations {
                    self.locals[*index] = Some(Value::U64(iteration));
                    if let Flow::Return(value) = self.block(body)? {
                        return Ok(Flow::Return(value));
                    }
                }
            }
            Stmt::For {
                item,
                list,
                bound,
                body,
            } => {
                let items = match self.eval(list)? {
                    Value::List(items) => items,
                    other => panic!("the checker types a for loop's list, found {other:?}"),
                };
                assert!(
                    items.len() as u64 <= bound.value,
                    "a list holds at most its capacity"
                );
                for element in items {
                    self.locals[*item] = Some(element);
                    if let Flow::Return(value) = self.block(body)? {
                        return Ok(Flow::Return(value));
                    }
                }
            }
            Stmt::Parallel(parallel) => self.parallel(parallel)?,
            Stmt::Effect { site, args, result } => {
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval(arg)?);
                }
                let value = self.effect(*site, values)?;
                self.locals[*result] = Some(value);
            }
            Stmt::Take { result } => {
                let steers = self.take();
                self.locals[*result] = Some(steers);
            }
            Stmt::Finish { value } => return Ok(Flow::Return(self.eval(value)?)),
        }
        Ok(Flow::Next)
    }

    fn parallel(&mut self, parallel: &Parallel) -> Result<(), Halt> {
        let items = match self.eval(&parallel.list)? {
            Value::List(items) => items,
            other => panic!("the checker types a parallel for's list, found {other:?}"),
        };
        assert!(
            items.len() as u64 <= parallel.bound.value,
            "a list holds at most its capacity"
        );
        let mut values: Vec<Option<Value>> = vec![None; items.len()];
        let mut pending = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            if let Some(waiting) = self.batch_call(parallel, index, item, &mut values)? {
                pending.push(waiting);
            }
        }
        let mut start = 0;
        while start < pending.len() {
            let alone = pending[start].call.effect.may_be_unknown();
            let mut end = start + 1;
            while !alone && end < pending.len() && !pending[end].call.effect.may_be_unknown() {
                end += 1;
            }
            self.batch_group(parallel, &pending[start..end], &mut values)?;
            start = end;
        }
        let mut ordered: Vec<(usize, Value)> = items.into_iter().enumerate().collect();
        if self.mutation == Mutation::BatchPushesReversed {
            ordered.reverse();
        }
        for (index, item) in ordered {
            let value = values[index]
                .take()
                .expect("every item of a parallel for has a result before its pushes");
            self.locals[parallel.item] = Some(item);
            self.locals[parallel.result] = Some(value);
            if let Flow::Return(_) = self.block(&parallel.body)? {
                panic!("the body of a parallel for only pushes");
            }
        }
        Ok(())
    }

    fn batch_call(
        &mut self,
        parallel: &Parallel,
        index: usize,
        item: &Value,
        values: &mut [Option<Value>],
    ) -> Result<Option<Pending>, Halt> {
        let site = &self.handler.sites[parallel.site];
        self.seq += 1;
        let seq = self.seq;
        let call = self.resolve(site, std::slice::from_ref(item));
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &recorded, &call)?;
                let site = site.clone();
                values[index] = Some(self.settle(&site, &call, &output)?);
                Ok(None)
            }
            Some(SiteLog::Reply(..)) => Err(self.diverged(seq, "a model reply", &call.tool)),
            Some(SiteLog::Unknown(recorded)) => {
                self.same(seq, &recorded, &call)?;
                values[index] = Some(Value::Variant(ir::UNKNOWN, Vec::new()));
                Ok(None)
            }
            Some(SiteLog::Open(recorded)) => {
                self.same(seq, &recorded, &call)?;
                self.require_live(seq)?;
                let in_step = matches!(self.target.recovery, Recovery::InStep { .. });
                assert!(
                    !call.effect.may_be_unknown()
                        || in_step
                        || matches!(
                            self.mutation,
                            Mutation::RecoveryRedispatchesExternal
                                | Mutation::RecoveryReportsFailed
                        ),
                    "recovery turns every open external call unknown before the turn runs again"
                );
                Ok(Some(Pending {
                    index,
                    seq,
                    call,
                    reopened: true,
                }))
            }
            None => {
                self.require_live(seq)?;
                self.append(Record::Call {
                    turn: self.turn,
                    seq,
                    tool: call.tool.clone(),
                    effect: call.effect,
                    input: call.input.clone(),
                });
                self.sites.insert(seq, SiteLog::Open(call.clone()));
                Ok(Some(Pending {
                    index,
                    seq,
                    call,
                    reopened: false,
                }))
            }
        }
    }

    fn batch_group(
        &mut self,
        parallel: &Parallel,
        group: &[Pending],
        values: &mut [Option<Value>],
    ) -> Result<(), Halt> {
        let external = group[0].call.effect.may_be_unknown();
        assert!(
            !external || group.len() == 1,
            "an external call of a parallel for runs alone"
        );
        let gate = self.target.gate_before_outbound;
        let in_step = external && matches!(self.target.recovery, Recovery::InStep { .. });
        let late = external && !gate && self.mutation == Mutation::BatchFlushAfterDispatch;
        if (external || gate) && !late {
            self.flush()?;
            assert!(
                self.buffer.is_empty(),
                "every call record of a parallel for is durable before an external call of it runs"
            );
        }
        if in_step {
            let first = &group[0];
            let attempt = match &mut self.mode {
                Mode::Live { world, .. } => world.attempt(self.turn, first.seq)?,
                Mode::Replay => unreachable!("replay never runs an effect"),
            };
            if first.reopened && attempt > 1 && self.mutation != Mutation::VercelRetryRedispatches {
                values[first.index] = Some(self.unknown(first.seq, first.call.clone())?);
                return Ok(());
            }
            assert!(
                first.reopened || attempt == 1,
                "a fresh external step starts at its first attempt"
            );
        }
        let completed = self.batch_dispatch(group)?;
        let replayed = !matches!(self.sites.get(&self.seq), Some(SiteLog::Open(_)));
        if !replayed || self.mutation == Mutation::BatchDrainsReplayed {
            self.drain_at(Drain::AfterRun)?;
        }
        let site = self.handler.sites[parallel.site].clone();
        for (position, output) in completed {
            let waiting = &group[position];
            self.append(Record::Result {
                turn: self.turn,
                seq: waiting.seq,
                output: output.clone(),
            });
            self.sites.insert(
                waiting.seq,
                SiteLog::Done(waiting.call.clone(), output.clone()),
            );
            values[waiting.index] = Some(self.settle(&site, &waiting.call, &output)?);
        }
        if late || in_step {
            self.flush()?;
        }
        Ok(())
    }

    fn batch_dispatch(&mut self, group: &[Pending]) -> Result<Vec<(usize, Output)>, Halt> {
        let returned: Vec<Type> = group
            .iter()
            .map(|waiting| self.dispatched_returns(&waiting.call.tool))
            .collect();
        let gates: Vec<Result<Option<String>, String>> = group
            .iter()
            .map(|waiting| self.dispatch_gate(&waiting.call))
            .collect();
        let sent: Vec<usize> = (0..group.len())
            .filter(|position| gates[*position].is_ok())
            .collect();
        let requests: Vec<ToolRequest<'_>> = sent
            .iter()
            .map(|position| {
                let waiting = &group[*position];
                ToolRequest {
                    turn: self.turn,
                    seq: waiting.seq,
                    tool: &waiting.call.tool,
                    effect: waiting.call.effect,
                    input: &waiting.call.input,
                    returns: &returned[*position],
                    admitted: gates[*position].as_ref().ok().and_then(Option::as_deref),
                    nested: None,
                }
            })
            .collect();
        let limit = self
            .target
            .limits
            .concurrent_requests
            .map(|limit| limit.value);
        let Mode::Live { world, .. } = &mut self.mode else {
            unreachable!("replay never runs an effect");
        };
        let mut completed = if requests.is_empty() {
            Vec::new()
        } else {
            world.tools(&requests, limit)?
        };
        assert_eq!(
            completed.len(),
            sent.len(),
            "a batch returns one output per request"
        );
        for (position, _) in &mut completed {
            *position = sent[*position];
        }
        for (position, gate) in gates.into_iter().enumerate() {
            if let Err(refused) = gate {
                completed.push((position, Output::Failed(refused)));
            }
        }
        if self.mutation != Mutation::ResultsInCompletionOrder {
            completed.sort_by_key(|(position, _)| *position);
        }
        let checked = completed
            .into_iter()
            .map(|(position, output)| {
                let tool = &group[position].call.tool;
                (position, self.checked(tool, &returned[position], output))
            })
            .collect();
        Ok(checked)
    }

    fn push(&mut self, field: usize, value: &Expr) -> Result<(), Halt> {
        let value = self.eval(value)?;
        let Type::List { capacity, .. } = &self.program.state[field].ty else {
            panic!("the checker pushes only onto lists");
        };
        let Value::List(items) = &mut self.state[field] else {
            panic!("a list field holds a list");
        };
        if items.len() as u64 >= capacity.value {
            return Err(Halt::Trap(format!(
                "bound({})",
                self.program.state[field].name
            )));
        }
        items.push(value);
        Ok(())
    }

    fn shrink(&mut self, field: usize, kept: &Kept) -> Result<(), Halt> {
        let (count, first) = match kept {
            Kept::Nothing => (0, None),
            Kept::Last(count) => (self.eval(count)?.as_u64(), None),
            Kept::LastAfter { count, first } => {
                (self.eval(count)?.as_u64(), Some(self.eval(first)?))
            }
        };
        let Type::List { capacity, .. } = &self.program.state[field].ty else {
            panic!("the checker clears and keeps only lists");
        };
        let capacity = capacity.value;
        let Value::List(items) = &mut self.state[field] else {
            panic!("the checker clears and keeps only lists");
        };
        let length = items.len() as u64;
        if let Some(first) = first {
            if count.min(length) >= capacity {
                return Err(Halt::Trap(format!(
                    "bound({})",
                    self.program.state[field].name
                )));
            }
            let dropped =
                usize::try_from(length - count.min(length)).expect("a list's length fits usize");
            let retired: Vec<Value> = items.drain(..dropped).collect();
            items.insert(0, first);
            assert_eq!(
                items.len() as u64,
                count.min(length) + 1,
                "keep after leaves the first entry and at most the last count entries"
            );
            self.retired += retired.len() as u64;
            if self.mutation == Mutation::RetireSkipped {
                self.unretired.entry(field).or_default().extend(retired);
            }
            return Ok(());
        }
        if count >= length {
            return Ok(());
        }
        let dropped = usize::try_from(length - count).expect("a list's length fits usize");
        let retired: Vec<Value> = items.drain(..dropped).collect();
        assert_eq!(
            items.len() as u64,
            count,
            "keep leaves exactly the last count entries"
        );
        assert!(!retired.is_empty(), "a shrink that runs retires an entry");
        self.retired += retired.len() as u64;
        if self.mutation == Mutation::RetireSkipped {
            self.unretired.entry(field).or_default().extend(retired);
        }
        Ok(())
    }

    fn eval(&mut self, expr: &Expr) -> Result<Value, Halt> {
        let cancelled = if reads_cancelled(expr) {
            Some(self.cancelled())
        } else {
            None
        };
        let reads = Reads {
            cancelled,
            uncut: self.mutation == Mutation::TruncateUncut,
            notices: Some(&self.notices),
        };
        evaluate_with(self.program, &self.state, &self.locals, reads, expr).map_err(Halt::Trap)
    }

    fn resolve(&self, site: &Site, args: &[Value]) -> Call {
        match site.kind {
            SiteKind::Tool(tool_id) => {
                let tool = &self.program.tools[tool_id];
                let mut input = BTreeMap::new();
                for (param, arg) in tool.params.iter().zip(args) {
                    input.insert(
                        param.name.clone(),
                        value::to_json(self.program, &param.ty, arg),
                    );
                }
                Call {
                    tool: tool.name.clone(),
                    effect: tool.effect,
                    input: Json::Object(input),
                }
            }
            SiteKind::Listing(listing) => Call {
                tool: self.program.listings[listing].name.clone(),
                effect: EffectClass::None,
                input: Json::Null,
            },
            SiteKind::Dispatch => {
                let (_, fields) = args[0].as_variant();
                let tool = fields[ir::CALL_TOOL].as_text().to_owned();
                let raw = fields[ir::CALL_INPUT].as_text();
                let dropped = self.mutation == Mutation::ListedClassDropped
                    && self.program.listed(&tool).is_some();
                let effect = if dropped {
                    EffectClass::None
                } else {
                    self.program.dispatch_class(&tool)
                };
                let input = match value::tool_input(self.program, &tool, raw) {
                    value::Input::Typed(_) if self.mutation == Mutation::DefaultUnfilled => {
                        crate::run::json::decode(raw).expect("typed input decoded once already")
                    }
                    value::Input::Typed(input) => input,
                    value::Input::Untyped | value::Input::Invalid(_) => {
                        Json::String(raw.to_owned())
                    }
                };
                Call {
                    tool,
                    effect,
                    input,
                }
            }
            SiteKind::Model => {
                let model = self
                    .program
                    .model
                    .as_ref()
                    .expect("the checker admits model calls only with a model");
                Call {
                    tool: model.name.clone(),
                    effect: EffectClass::Idempotent,
                    input: Json::Null,
                }
            }
            SiteKind::Now => Call {
                tool: "now".to_owned(),
                effect: EffectClass::None,
                input: Json::Null,
            },
            SiteKind::Random => Call {
                tool: "random".to_owned(),
                effect: EffectClass::None,
                input: Json::Null,
            },
            SiteKind::Cell(code) => Call {
                tool: self.program.codes[code].name.clone(),
                effect: EffectClass::None,
                input: Json::String(args[0].as_text().to_owned()),
            },
            SiteKind::Wait(_) => Call {
                tool: ir::WAIT_TOOL.to_owned(),
                effect: EffectClass::None,
                input: Json::object([(WAIT_MS, Json::Number(args[0].as_u64()))]),
            },
            SiteKind::Suspend => Call {
                tool: ir::SUSPEND_TOOL.to_owned(),
                effect: EffectClass::None,
                input: Json::object([(SUSPEND_KEY, Json::String(args[0].as_text().to_owned()))]),
            },
        }
    }

    fn effect(&mut self, site_id: usize, args: Vec<Value>) -> Result<Value, Halt> {
        let site = &self.handler.sites[site_id];
        if let SiteKind::Wait(cap) = site.kind {
            let cap = &self.program.constants[cap];
            if args[0].as_u64() > cap.value {
                return Err(Halt::Trap(format!("bound({})", cap.name)));
            }
        }
        self.seq += 1;
        let seq = self.seq;
        let call = self.resolve(site, &args);
        let cell = match site.kind {
            SiteKind::Cell(code) => Some(code),
            SiteKind::Dispatch => self.program.code(&call.tool).map(|(code, _)| code),
            _ => None,
        };
        if let Some(code) = cell {
            let call = Call {
                effect: EffectClass::None,
                ..call
            };
            return self.cell_site(code, seq, call);
        }
        if site.kind == SiteKind::Suspend {
            return self.suspend_site(site_id, seq, call);
        }
        if let SiteKind::Wait(_) = site.kind {
            return self.wait_site(site_id, seq, call, &args);
        }
        if let SiteClass::Fixed(class) = site.class {
            assert_eq!(
                class, call.effect,
                "a fixed site runs with its declared class"
            );
        }
        let replayed = matches!(
            self.sites.get(&seq),
            Some(SiteLog::Done(..) | SiteLog::Reply(..))
        );
        if replayed && self.mutation == Mutation::EventDrainOnReplay {
            self.drain()?;
        }
        let site = &self.handler.sites[site_id];
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &recorded, &call)?;
                let site = site.clone();
                self.settle(&site, &call, &output)
            }
            Some(SiteLog::Reply(response, recorded)) => {
                if site.kind != SiteKind::Model {
                    return Err(self.diverged(seq, "a model reply", &call.tool));
                }
                let mode = site.mode.map(|mode| self.program.mode_name(mode));
                if recorded.as_deref() != mode {
                    return Err(self.diverged(
                        seq,
                        &format!("a model reply in {}", mode_text(recorded.as_deref())),
                        &format!("{} in {}", call.tool, mode_text(mode)),
                    ));
                }
                self.reply(&response)
            }
            Some(SiteLog::Unknown(recorded)) => {
                self.same(seq, &recorded, &call)?;
                if !site.result.reports_unknown(self.program) {
                    return self.reopen(site_id, seq, call, &args);
                }
                Ok(Value::Variant(ir::UNKNOWN, Vec::new()))
            }
            Some(SiteLog::Open(recorded)) => {
                self.same(seq, &recorded, &call)?;
                self.reopen(site_id, seq, call, &args)
            }
            None => {
                if site.kind == SiteKind::Model {
                    self.require_live(seq)?;
                    return self.execute(site_id, seq, &call, &args);
                }
                self.require_live(seq)?;
                self.append(Record::Call {
                    turn: self.turn,
                    seq,
                    tool: call.tool.clone(),
                    effect: call.effect,
                    input: call.input.clone(),
                });
                self.sites.insert(seq, SiteLog::Open(call.clone()));
                self.execute(site_id, seq, &call, &args)
            }
        }
    }

    fn suspend_site(&mut self, site_id: usize, seq: u64, call: Call) -> Result<Value, Halt> {
        let live = matches!(self.mode, Mode::Live { .. });
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &recorded, &call)?;
                if live && self.mutation == Mutation::ResumeReplaysSuspend {
                    return self.suspend_fresh(seq, call);
                }
                let site = &self.handler.sites[site_id];
                self.decode(site, &call, &output)
            }
            Some(SiteLog::Open(recorded) | SiteLog::Unknown(recorded)) => {
                self.same(seq, &recorded, &call)?;
                self.require_live(seq)?;
                if let Some(answer) = self.take_resume(seq, &recorded)? {
                    let site = &self.handler.sites[site_id];
                    return self.decode(site, &call, &answer);
                }
                self.suspend(seq, &recorded)
            }
            Some(SiteLog::Reply(..)) => Err(self.diverged(seq, "a model reply", &call.tool)),
            None => {
                self.require_live(seq)?;
                self.suspend_fresh(seq, call)
            }
        }
    }

    fn take_resume(&mut self, seq: u64, call: &Call) -> Result<Option<Output>, Halt> {
        let key = call
            .input
            .get(SUSPEND_KEY)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned();
        let suspension = Suspension {
            turn: self.turn,
            seq,
            key,
        };
        let Some(resume) = self.world().resume(&suspension) else {
            return Ok(None);
        };
        if !self.resume(&suspension, &resume).map_err(Halt::Exit)? {
            return Ok(None);
        }
        Ok(Some(Output::Ok(Json::String(resume.answer))))
    }

    fn suspend_fresh(&mut self, seq: u64, call: Call) -> Result<Value, Halt> {
        self.append(Record::Call {
            turn: self.turn,
            seq,
            tool: call.tool.clone(),
            effect: call.effect,
            input: call.input.clone(),
        });
        self.sites.insert(seq, SiteLog::Open(call.clone()));
        self.suspend(seq, &call)
    }

    fn suspend(&mut self, seq: u64, call: &Call) -> Result<Value, Halt> {
        if self.mutation != Mutation::SuspendBeforeFlush {
            self.flush()?;
            assert!(
                self.buffer.is_empty(),
                "a suspend's call record is durable before the turn waits"
            );
        }
        self.hook(Point::BeforeRun(SiteKind::Suspend, EffectClass::None))?;
        let key = call
            .input
            .get(SUSPEND_KEY)
            .and_then(Json::as_str)
            .ok_or_else(|| {
                Halt::Exit(Exit::Corrupt(format!(
                    "turn {} seq {seq}: a suspend's input lacks its key",
                    self.turn
                )))
            })?
            .to_owned();
        Err(Halt::Suspend(Suspension {
            turn: self.turn,
            seq,
            key,
        }))
    }

    fn wait_site(
        &mut self,
        site_id: usize,
        seq: u64,
        call: Call,
        args: &[Value],
    ) -> Result<Value, Halt> {
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &without_until(&recorded), &call)?;
                let site = &self.handler.sites[site_id];
                self.decode(site, &call, &output)
            }
            Some(SiteLog::Open(recorded) | SiteLog::Unknown(recorded)) => {
                self.same(seq, &without_until(&recorded), &call)?;
                self.require_live(seq)?;
                if self.mutation == Mutation::WaitRerunSkips {
                    let clock = self.world().clock();
                    return self.waited(site_id, seq, &recorded, clock);
                }
                self.reopen(site_id, seq, recorded, args)
            }
            Some(SiteLog::Reply(..)) => Err(self.diverged(seq, "a model reply", &call.tool)),
            None => {
                self.require_live(seq)?;
                let ms = wait_ms(&call.input).expect("a wait's input holds its duration");
                let clock = self.world().clock();
                let until = clock.checked_add(ms).ok_or_else(|| {
                    Halt::Exit(Exit::Corrupt(format!(
                        "turn {} seq {seq}: the clock {clock} plus the wait's {ms} ms passes u64",
                        self.turn
                    )))
                })?;
                let mut input = BTreeMap::new();
                input.insert(WAIT_MS.to_owned(), Json::Number(ms));
                input.insert(WAIT_UNTIL.to_owned(), Json::Number(until));
                let call = Call {
                    input: Json::Object(input),
                    ..call
                };
                self.append(Record::Call {
                    turn: self.turn,
                    seq,
                    tool: call.tool.clone(),
                    effect: call.effect,
                    input: call.input.clone(),
                });
                self.sites.insert(seq, SiteLog::Open(call.clone()));
                self.execute(site_id, seq, &call, args)
            }
        }
    }

    fn sleep(&mut self, seq: u64, call: &Call) -> Result<u64, Halt> {
        let until = wait_until(&call.input).ok_or_else(|| {
            Halt::Exit(Exit::Corrupt(format!(
                "turn {} seq {seq}: a wait's input lacks its deadline",
                self.turn
            )))
        })?;
        let ms = wait_ms(&call.input).unwrap_or(0);
        let until = if self.mutation == Mutation::WaitOversleeps {
            until.saturating_add(ms)
        } else {
            until
        };
        let turn = self.turn;
        Ok(self.world().sleep(turn, seq, until)?)
    }

    fn waited(&mut self, site_id: usize, seq: u64, call: &Call, clock: u64) -> Result<Value, Halt> {
        let output = Output::Ok(Json::Number(clock));
        if self.mutation != Mutation::WaitUnjournaled {
            self.append(Record::Result {
                turn: self.turn,
                seq,
                output: output.clone(),
            });
            self.sites
                .insert(seq, SiteLog::Done(call.clone(), output.clone()));
        }
        let site = &self.handler.sites[site_id];
        self.decode(site, &call, &output)
    }

    fn cell_site(&mut self, code: crate::ir::CodeId, seq: u64, call: Call) -> Result<Value, Halt> {
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &recorded, &call)?;
                if self.mutation == Mutation::CellRerunAfterFinish
                    && matches!(self.mode, Mode::Live { .. })
                    && let Some(log) = self.cells.get(&seq).cloned()
                {
                    cell_start_observed(self, log.cell, &log.header.script);
                }
                return self.decode_cell(&output);
            }
            Some(SiteLog::Unknown(recorded)) => {
                self.same(seq, &recorded, &call)?;
                return Ok(Value::Variant(ir::UNKNOWN, Vec::new()));
            }
            Some(SiteLog::Reply(..)) => {
                return Err(self.diverged(seq, "a model reply", &call.tool));
            }
            Some(SiteLog::Open(recorded)) => {
                self.same(seq, &recorded, &call)?;
                self.require_live(seq)?;
            }
            None => {
                self.require_live(seq)?;
                self.append(Record::Call {
                    turn: self.turn,
                    seq,
                    tool: call.tool.clone(),
                    effect: call.effect,
                    input: call.input.clone(),
                });
                self.sites.insert(seq, SiteLog::Open(call.clone()));
            }
        }
        let outcome = self.run_cell(code, seq, &call)?;
        let output = outcome.output();
        self.append(Record::Result {
            turn: self.turn,
            seq,
            output: output.clone(),
        });
        self.sites.insert(seq, SiteLog::Done(call, output.clone()));
        self.decode_cell(&output)
    }

    fn run_cell(
        &mut self,
        code: crate::ir::CodeId,
        seq: u64,
        call: &Call,
    ) -> Result<CellOutcome, Halt> {
        let script = call
            .input
            .as_str()
            .expect("a cell's input is its script")
            .to_owned();
        let existing = self.cells.get(&seq).cloned();
        if let Some(log) = &existing
            && log.finished
        {
            return Ok(CellOutcome::Unknown(divergence_text(
                log.cell,
                "it finished but its result was lost with the journal's tail",
            )));
        }
        let fresh = self.next_cell;
        assert!(fresh > 0, "a live turn knows its session's next cell id");
        let nested = self.nested.clone();
        let session = self.program.name.clone();
        let site = cell::CellSite {
            program: self.program,
            code,
            target: self.target,
            mutation: self.mutation,
            session: &session,
            turn: self.turn,
            seq,
            script: &script,
            cell: match &existing {
                Some(log) => cell::Cell::Existing(log),
                None => cell::Cell::Fresh(fresh),
            },
            nested: &nested,
        };
        if existing.is_none() {
            self.next_cell += 1;
        }
        cell::run(site, self).map_err(Halt::from)
    }

    fn decode_cell(&self, output: &Output) -> Result<Value, Halt> {
        let outcome = CellOutcome::from_output(output).map_err(|error| {
            Halt::Exit(Exit::Corrupt(format!("turn {} result: {error}", self.turn)))
        })?;
        Ok(match outcome {
            CellOutcome::Done(text) => Value::Variant(ir::OK, vec![Value::Text(text)]),
            CellOutcome::Threw(error) => Value::Variant(ir::FAILED, vec![Value::Text(error)]),
            CellOutcome::Unknown(_) => Value::Variant(ir::UNKNOWN, Vec::new()),
        })
    }

    fn world(&mut self) -> &mut dyn World {
        match &mut self.mode {
            Mode::Live { world, .. } => &mut **world,
            Mode::Replay => unreachable!("cells, waits and resumes run only live"),
        }
    }

    fn require_live(&self, seq: u64) -> Result<(), Halt> {
        match self.mode {
            Mode::Live { .. } => Ok(()),
            Mode::Replay => Err(Halt::Exit(Exit::Corrupt(format!(
                "replay of turn {} needs seq {seq}, which the journal does not record",
                self.turn
            )))),
        }
    }

    fn reopen(
        &mut self,
        site_id: usize,
        seq: u64,
        call: Call,
        args: &[Value],
    ) -> Result<Value, Halt> {
        self.require_live(seq)?;
        let in_step = matches!(self.target.recovery, Recovery::InStep { .. });
        if call.effect.may_be_unknown() && in_step {
            let attempt = match &mut self.mode {
                Mode::Live { world, .. } => world.attempt(self.turn, seq)?,
                Mode::Replay => unreachable!("replay never reopens a call"),
            };
            if attempt > 1 && self.mutation != Mutation::VercelRetryRedispatches {
                return self.unknown(seq, call);
            }
            return self.dispatch(site_id, seq, &call, args, true);
        }
        assert!(
            !call.effect.may_be_unknown()
                || matches!(
                    self.mutation,
                    Mutation::RecoveryRedispatchesExternal | Mutation::RecoveryReportsFailed
                ),
            "recovery turns every open external call unknown before the turn runs again"
        );
        if self.mutation == Mutation::RerunRewritesCall && !call.effect.may_be_unknown() {
            self.append(Record::Call {
                turn: self.turn,
                seq,
                tool: call.tool.clone(),
                effect: call.effect,
                input: call.input.clone(),
            });
        }
        self.execute(site_id, seq, &call, args)
    }

    fn unknown(&mut self, seq: u64, call: Call) -> Result<Value, Halt> {
        let attempt = self.next_attempt();
        self.append(Record::Recovery {
            turn: self.turn,
            attempt,
            unknown: vec![seq],
            nested: Vec::new(),
        });
        self.sites.insert(seq, SiteLog::Unknown(call));
        self.flush()?;
        Ok(Value::Variant(ir::UNKNOWN, Vec::new()))
    }

    fn execute(
        &mut self,
        site_id: usize,
        seq: u64,
        call: &Call,
        args: &[Value],
    ) -> Result<Value, Halt> {
        let site = &self.handler.sites[site_id];
        let gate = self.target.gate_before_outbound && site.kind.outbound();
        let required = site.durability.flushes(call.effect);
        let late = match self.mutation {
            Mutation::FlushAfterDispatch => required && !gate,
            Mutation::GateReleasesEarly => gate,
            _ => false,
        };
        if (required || gate) && !late {
            self.flush()?;
            assert!(
                self.buffer.is_empty(),
                "the call record is durable before its effect runs"
            );
        }
        let in_step =
            matches!(self.target.recovery, Recovery::InStep { .. }) && call.effect.may_be_unknown();
        if in_step {
            let attempt = match &mut self.mode {
                Mode::Live { world, .. } => world.attempt(self.turn, seq)?,
                Mode::Replay => unreachable!("replay never runs an effect"),
            };
            assert_eq!(
                attempt, 1,
                "a fresh external step starts at its first attempt"
            );
        }
        let value = self.dispatch(site_id, seq, call, args, in_step)?;
        if late {
            self.flush()?;
        }
        Ok(value)
    }

    fn dispatch(
        &mut self,
        site_id: usize,
        seq: u64,
        call: &Call,
        args: &[Value],
        step: bool,
    ) -> Result<Value, Halt> {
        let site = self.handler.sites[site_id].clone();
        self.hook(Point::BeforeRun(site.kind, call.effect))?;
        let gate = self.gate(&site, call);
        let Mode::Live { world, .. } = &mut self.mode else {
            unreachable!("replay never runs an effect");
        };
        let output = match site.kind {
            SiteKind::Model => {
                let calls_max = self
                    .program
                    .model
                    .as_ref()
                    .map_or(0, |model| model.calls.value);
                let mode = site
                    .mode
                    .map(|mode| self.program.mode_name(mode).to_owned());
                let sent = mode
                    .as_deref()
                    .filter(|_| self.mutation != Mutation::ModeDropped);
                let response = world.model(&ModelRequest {
                    turn: self.turn,
                    seq,
                    calls_max,
                    mode: sent,
                    context: &args[0],
                    blobs: &self.blobs,
                });
                self.hook(Point::AfterRun(site.kind, call.effect))?;
                self.drain_at(Drain::AfterRun)?;
                if self.reply_unrecorded(&response) {
                    return self.reply(&response);
                }
                let recorded = if self.mutation == Mutation::RetryAfterUnjournaled {
                    value::without_retry_after(&response)
                } else {
                    response.clone()
                };
                self.append(Record::Model {
                    turn: self.turn,
                    seq,
                    response: recorded.clone(),
                    mode: mode.clone(),
                });
                self.sites.insert(seq, SiteLog::Reply(recorded, mode));
                return self.reply(&response);
            }
            SiteKind::Now => Output::Ok(Json::Number(world.now())),
            SiteKind::Random => Output::Ok(Json::Number(world.random())),
            SiteKind::Cell(_) => unreachable!("a cell site runs through the cell runner"),
            SiteKind::Suspend => unreachable!("a suspend ends the run before it would run"),
            SiteKind::Wait(_) => {
                let woke = self.sleep(seq, call)?;
                self.hook(Point::AfterRun(site.kind, call.effect))?;
                self.drain_at(Drain::AfterRun)?;
                return self.waited(site_id, seq, call, woke);
            }
            SiteKind::Listing(listing) => {
                let listing = &self.program.listings[listing];
                let output = world.listing(&ListingRequest {
                    turn: self.turn,
                    seq,
                    listing: &listing.name,
                });
                value::checked_listing(listing, output)
            }
            SiteKind::Tool(_) | SiteKind::Dispatch => {
                let returns = match &site.result {
                    Type::Known(inner) | Type::Maybe(inner) => (**inner).clone(),
                    other if other.reports_unknown(self.program) => {
                        self.dispatched_returns(&call.tool)
                    }
                    other => panic!("a tool site yields known or maybe, found {other:?}"),
                };
                let Mode::Live { world, .. } = &mut self.mode else {
                    unreachable!("replay never runs an effect");
                };
                match gate {
                    Err(refused) => Output::Failed(refused),
                    Ok(admitted) => {
                        let output = world.tool(&ToolRequest {
                            turn: self.turn,
                            seq,
                            tool: &call.tool,
                            effect: call.effect,
                            input: &call.input,
                            returns: &returns,
                            admitted: admitted.as_deref(),
                            nested: None,
                        });
                        self.checked(&call.tool, &returns, output)
                    }
                }
            }
        };
        self.hook(Point::AfterRun(site.kind, call.effect))?;
        self.drain_at(Drain::AfterRun)?;
        self.append(Record::Result {
            turn: self.turn,
            seq,
            output: output.clone(),
        });
        self.sites
            .insert(seq, SiteLog::Done(call.clone(), output.clone()));
        if step {
            self.flush()?;
        }
        self.settle(&site, call, &output)
    }

    fn dispatched_returns(&self, tool: &str) -> Type {
        match crate::run::blob::returned_bound(self.program, tool) {
            Some(bytes) if self.program.blobs.is_some() => Type::Blob(bytes.clone()),
            _ => Type::Text,
        }
    }

    fn checked(&mut self, tool: &str, returns: &Type, output: Output) -> Output {
        let output = if self.mutation == Mutation::EnumUnchecked {
            value::fallback_output(self.program, returns, output)
        } else {
            value::checked_output(self.program, tool, returns, output)
        };
        let output = match self.program.search_of(tool) {
            Some(listing) if self.mutation != Mutation::SearchUnchecked => value::checked_search(
                self.program,
                tool,
                listing,
                self.admitted.get(&listing),
                output,
            ),
            _ => output,
        };
        match (returns, output) {
            (Type::Blob(_), Output::Ok(Json::String(data))) => {
                let with_data = self.mutation != Mutation::BlobUnjournaled;
                Output::Ok(self.blobs.journaled(&data, with_data))
            }
            (_, output) => output,
        }
    }

    fn settle(&mut self, site: &Site, call: &Call, output: &Output) -> Result<Value, Halt> {
        self.admit_listing(site, output)?;
        if let (Some(listing), Output::Ok(Json::String(text))) =
            (self.program.search_of(&call.tool), output)
        {
            let found = self.discovered.entry(listing).or_default();
            for name in value::found_names(text) {
                if let Some((_, bare)) = self.program.listed(name) {
                    found.insert(bare.to_owned());
                }
            }
        }
        self.decode(site, call, output)
    }

    fn gate(&self, site: &Site, call: &Call) -> Result<Option<String>, String> {
        if site.kind != SiteKind::Dispatch {
            return Ok(None);
        }
        self.dispatch_gate(call)
    }

    fn dispatch_gate(&self, call: &Call) -> Result<Option<String>, String> {
        if let Json::String(raw) = &call.input
            && let value::Input::Invalid(error) = value::tool_input(self.program, &call.tool, raw)
            && self.mutation != Mutation::InputUnchecked
        {
            return Err(error);
        }
        let Some((listing, name)) = self.program.listed(&call.tool) else {
            return Ok(None);
        };
        if self.mutation == Mutation::ListingUnchecked {
            return Ok(None);
        }
        let Some(fingerprint) = self
            .admitted
            .get(&listing)
            .and_then(|tools| tools.get(name))
        else {
            return Err(value::not_admitted(
                &call.tool,
                &self.program.listings[listing].name,
                name,
            ));
        };
        let found = self
            .discovered
            .get(&listing)
            .is_some_and(|found| found.contains(name));
        if self.program.deferred(listing) && !found && self.mutation != Mutation::DiscoveryUnchecked
        {
            return Err(value::not_discovered(
                &call.tool,
                &self.program.listings[listing].name,
            ));
        }
        Ok(Some(fingerprint.clone()))
    }

    fn admit_listing(&mut self, site: &Site, output: &Output) -> Result<(), Halt> {
        let SiteKind::Listing(listing) = site.kind else {
            return Ok(());
        };
        match output {
            Output::Ok(json) => {
                let tools = value::admitted(json).map_err(|error| {
                    Halt::Exit(Exit::Corrupt(format!("turn {} result: {error}", self.turn)))
                })?;
                let name = &self.program.listings[listing].name;
                let notice = match self.listed.get(&listing) {
                    Some(before) if self.mutation != Mutation::NoticeDropped => {
                        value::notice(name, before, &tools)
                    }
                    _ => String::new(),
                };
                self.notices.insert(listing, notice);
                self.listed.insert(listing, tools.clone());
                self.admitted.insert(listing, tools);
            }
            Output::Failed(_) => {
                self.notices.insert(listing, String::new());
                self.admitted.remove(&listing);
            }
        }
        Ok(())
    }

    fn reply(&self, response: &Json) -> Result<Value, Halt> {
        let model = self
            .program
            .model
            .as_ref()
            .expect("the checker admits model calls only with a model");
        match model.outcome() {
            ir::ModelOutcome::Attempt => {
                let checked = self.mutation != Mutation::UsageUnchecked;
                return Ok(value::attempt(self.program, response, checked));
            }
            ir::ModelOutcome::Known => return Ok(value::model_outcome(self.program, response)),
            ir::ModelOutcome::Trap => {}
        }
        value::from_json(self.program, &Type::Sum(ir::REPLY), response)
            .map_err(|error| Halt::Trap(format!("model reply: {error}")))
    }

    fn reply_unrecorded(&self, response: &Json) -> bool {
        self.mutation == Mutation::ModelFailureUnrecorded
            && self.program.model.as_ref().is_some_and(ir::Model::declared)
            && value::model_failure(response).is_some()
    }

    fn decode(&self, site: &Site, call: &Call, output: &Output) -> Result<Value, Halt> {
        let corrupt = |error: String| {
            Halt::Exit(Exit::Corrupt(format!("turn {} result: {error}", self.turn)))
        };
        if site.result.reports_unknown(self.program) && !matches!(site.result, Type::Maybe(_)) {
            return match output {
                Output::Failed(error) => {
                    Ok(Value::Variant(ir::FAILED, vec![Value::Text(error.clone())]))
                }
                Output::Ok(json)
                    if crate::run::blob::returned_bound(self.program, &call.tool).is_some() =>
                {
                    let digest = crate::run::blob::digest_of(json).map_err(corrupt)?;
                    Ok(Value::Variant(
                        ir::DISPATCHED_BLOB,
                        vec![Value::Blob(digest.to_owned())],
                    ))
                }
                Output::Ok(Json::String(text)) => {
                    Ok(Value::Variant(ir::OK, vec![Value::Text(text.clone())]))
                }
                Output::Ok(json) => Err(corrupt(format!("{} is not text", json.encode()))),
            };
        }
        if let (SiteKind::Listing(_), Output::Ok(json)) = (site.kind, output) {
            let tools = value::admitted(json).map_err(corrupt)?;
            return Ok(Value::Variant(ir::OK, vec![Value::U64(tools.len() as u64)]));
        }
        match (&site.result, output) {
            (Type::Text, Output::Ok(Json::String(text))) => Ok(Value::Text(text.clone())),
            (Type::U64, Output::Ok(json)) => json
                .as_u64()
                .map(Value::U64)
                .ok_or_else(|| corrupt(format!("{} is not u64", json.encode()))),
            (Type::Known(inner) | Type::Maybe(inner), Output::Ok(json)) => {
                let value = value::from_json(self.program, inner, json).map_err(corrupt)?;
                Ok(Value::Variant(ir::OK, vec![value]))
            }
            (Type::Known(_) | Type::Maybe(_), Output::Failed(error)) => {
                Ok(Value::Variant(ir::FAILED, vec![Value::Text(error.clone())]))
            }
            (ty, output) => Err(corrupt(format!("{output:?} does not fit {ty:?}"))),
        }
    }

    fn same(&self, seq: u64, recorded: &Call, call: &Call) -> Result<(), Halt> {
        if recorded.tool != call.tool {
            return Err(self.diverged(seq, &recorded.tool, &call.tool));
        }
        if recorded.effect != call.effect || recorded.input != call.input {
            return Err(Halt::Exit(Exit::Corrupt(format!(
                "replay of turn {} diverged at seq {seq}: tool '{}' was recorded with another class or input",
                self.turn, call.tool
            ))));
        }
        Ok(())
    }

    fn diverged(&self, seq: u64, recorded: &str, running: &str) -> Halt {
        Halt::Exit(Exit::Corrupt(format!(
            "replay of turn {} diverged at seq {seq}: the journal records {recorded}, the program runs '{running}' ({})",
            self.turn,
            call_id(self.turn, seq)
        )))
    }
}

fn mode_text(mode: Option<&str>) -> String {
    mode.map_or_else(|| "no mode".to_owned(), |mode| format!("mode {mode}"))
}

fn cell_start_observed(engine: &mut Engine<'_>, cell: u64, script: &str) {
    engine.world().cell_start(cell, script);
}

impl CellHost for Engine<'_> {
    fn append(&mut self, record: Record) {
        Engine::append(self, record);
    }

    fn flush(&mut self) -> Result<(), Stop> {
        Engine::flush(self).map_err(halt_stop)
    }

    fn flush_withholding(&mut self, cell: u64) -> Result<(), Stop> {
        let (withheld, rest): (Vec<Record>, Vec<Record>) = std::mem::take(&mut self.buffer)
            .into_iter()
            .partition(|record| matches!(record, Record::Cell { cell: held, .. } if *held == cell));
        self.buffer = rest;
        let flushed = Engine::flush(self).map_err(halt_stop);
        self.buffer.extend(withheld);
        flushed
    }

    fn hook(&mut self, point: CellPoint) -> Result<(), Stop> {
        self.world().hook(Point::Cell(point))
    }

    fn start(&mut self, cell: u64, script: &str) -> Step {
        self.world().cell_start(cell, script)
    }

    fn resume(&mut self, cell: u64, input: Input) -> Step {
        self.world().cell_resume(cell, input)
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        self.world().tool(request)
    }

    fn attempt(&mut self, seq: u64, nested: u64, first: bool) -> Result<u64, Stop> {
        let turn = self.turn;
        self.world().nested_attempt(turn, seq, nested, first)
    }

    fn recovered(&mut self, seq: u64, nested: u64) -> Result<(), Stop> {
        let attempt = self.next_attempt();
        self.append(Record::Recovery {
            turn: self.turn,
            attempt,
            unknown: Vec::new(),
            nested: vec![(seq, nested)],
        });
        if let Some(SiteLog::Open(call)) = self.nested.get(&(seq, nested)).cloned() {
            self.nested.insert((seq, nested), SiteLog::Unknown(call));
        }
        Engine::flush(self).map_err(halt_stop)
    }

    fn clock(&mut self) -> u64 {
        self.world().now()
    }

    fn random(&mut self) -> u64 {
        self.world().random()
    }

    fn wait(&mut self, wait: &Wait) -> Result<(), Stop> {
        self.world().wait(wait)
    }

    fn waited(&mut self, wait: &Wait) -> bool {
        self.world().waited(wait)
    }

    fn observe(&mut self, event: &CellEvent) {
        self.world().observe(event);
    }
}

fn halt_stop(halt: Halt) -> Stop {
    match halt {
        Halt::Exit(Exit::Stop(stop)) => stop,
        Halt::Exit(Exit::Corrupt(error)) => {
            panic!("a flush cannot find the journal corrupt, found {error}")
        }
        Halt::Trap(trap) => panic!("a flush cannot trap, found {trap}"),
        Halt::Suspend(_) => panic!("a flush cannot suspend"),
    }
}

pub const WAIT_MS: &str = "ms";
pub const WAIT_UNTIL: &str = "until";
pub const SUSPEND_KEY: &str = "key";

pub fn wait_ms(input: &Json) -> Option<u64> {
    input.get(WAIT_MS).and_then(Json::as_u64)
}

pub fn wait_until(input: &Json) -> Option<u64> {
    input.get(WAIT_UNTIL).and_then(Json::as_u64)
}

fn without_until(call: &Call) -> Call {
    let input = match &call.input {
        Json::Object(fields) => Json::Object(
            fields
                .iter()
                .filter(|(key, _)| key.as_str() != WAIT_UNTIL)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        other => other.clone(),
    };
    Call {
        input,
        ..call.clone()
    }
}

fn frozen(log: &TurnLog) -> Option<u64> {
    if log.sites.is_empty() {
        return None;
    }
    let replayed = log
        .sites
        .iter()
        .filter(|(_, site)| !matches!(site, SiteLog::Open(_)))
        .map(|(seq, _)| *seq)
        .max();
    Some(replayed.unwrap_or(0))
}

pub fn event_seq(records_max: u64, turn: u64, index: u64) -> Option<u64> {
    assert!(index >= 1, "an event's index in its turn counts from 1");
    turn.checked_sub(1)?
        .checked_mul(records_max)?
        .checked_add(index)
}

pub fn steer_count(events: &[Recorded]) -> u64 {
    events
        .iter()
        .filter(|event| matches!(event.delivery.inbound, Inbound::Steer(_)))
        .count() as u64
}

fn halt_exit(halt: Halt) -> Exit {
    match halt {
        Halt::Exit(exit) => exit,
        Halt::Trap(trap) => panic!("finishing a turn cannot trap, found {trap}"),
        Halt::Suspend(_) => panic!("finishing a turn cannot suspend"),
    }
}

fn reads_cancelled(expr: &Expr) -> bool {
    match expr {
        Expr::Cancelled => true,
        Expr::U64(_)
        | Expr::Bool(_)
        | Expr::Text(_)
        | Expr::Constant(_)
        | Expr::Local(_)
        | Expr::ListingNotice(_)
        | Expr::Field(_) => false,
        Expr::Member { base, .. } => reads_cancelled(base),
        Expr::Construct { fields, .. } => fields.iter().any(reads_cancelled),
        Expr::Binary { left, right, .. }
        | Expr::With {
            list: left,
            entry: right,
        } => reads_cancelled(left) || reads_cancelled(right),
        Expr::Not(operand)
        | Expr::Len(operand)
        | Expr::TextLen(operand)
        | Expr::Truncate { text: operand, .. } => reads_cancelled(operand),
        Expr::TextBuiltin { args, .. } => args.iter().any(reads_cancelled),
    }
}

#[derive(Clone, Copy)]
pub struct Reads<'r> {
    pub cancelled: Option<bool>,
    pub uncut: bool,
    pub notices: Option<&'r BTreeMap<ir::ListingId, String>>,
}

pub fn evaluate(
    program: &Program,
    state: &[Value],
    locals: &[Option<Value>],
    cancelled: Option<bool>,
    expr: &Expr,
) -> Result<Value, String> {
    let reads = Reads {
        cancelled,
        uncut: false,
        notices: None,
    };
    evaluate_with(program, state, locals, reads, expr)
}

pub fn evaluate_with(
    program: &Program,
    state: &[Value],
    locals: &[Option<Value>],
    reads: Reads<'_>,
    expr: &Expr,
) -> Result<Value, String> {
    let cancelled = reads.cancelled;
    let eval = |expr: &Expr| evaluate_with(program, state, locals, reads, expr);
    Ok(match expr {
        Expr::U64(value) => Value::U64(*value),
        Expr::Bool(value) => Value::Bool(*value),
        Expr::Text(value) => Value::Text(value.clone()),
        Expr::Constant(constant) => Value::U64(program.constants[*constant].value),
        Expr::Local(local) => locals[*local]
            .clone()
            .expect("the checker binds a local before its use"),
        Expr::Field(field) => state[*field].clone(),
        Expr::Member { base, index } => eval(base)?.as_variant().1[*index].clone(),
        Expr::Construct {
            variant, fields, ..
        } => {
            let mut values = Vec::with_capacity(fields.len());
            for field in fields {
                values.push(eval(field)?);
            }
            Value::Variant(*variant, values)
        }
        Expr::Binary {
            op, left, right, ..
        } => binary(*op, eval(left)?, eval(right)?)?,
        Expr::Not(operand) => Value::Bool(!eval(operand)?.as_bool()),
        Expr::Len(operand) => match eval(operand)? {
            Value::List(items) => Value::U64(items.len() as u64),
            other => panic!("the checker types len's operand as a list, found {other:?}"),
        },
        Expr::TextLen(operand) => Value::U64(ir::utf16_len(eval(operand)?.as_text())),
        Expr::Truncate { text, tokens } => {
            let text = eval(text)?;
            if reads.uncut {
                return Ok(text);
            }
            Value::Text(ir::truncate_middle(
                text.as_text(),
                program.constants[*tokens].value,
            ))
        }
        Expr::With { list, entry } => match eval(list)? {
            Value::List(mut items) => {
                items.push(eval(entry)?);
                Value::List(items)
            }
            other => panic!("the checker types a context as a list, found {other:?}"),
        },
        Expr::TextBuiltin { op, args } => {
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(eval(arg)?.as_text().to_owned());
            }
            let subject = values.first().expect("a text builtin reads its subject");
            match ir::text_op(*op, subject, values.get(1).map(String::as_str)) {
                ir::TextValue::Bool(flag) => Value::Bool(flag),
                ir::TextValue::Text(text) => Value::Text(text),
            }
        }
        Expr::Cancelled => {
            Value::Bool(cancelled.expect("a turn reads its events before it evaluates 'cancelled'"))
        }
        Expr::ListingNotice(listing) => Value::Text(
            reads
                .notices
                .expect("only a handler's body reads a listing's notice")
                .get(listing)
                .cloned()
                .unwrap_or_default(),
        ),
    })
}

fn binary(op: BinaryOp, left: Value, right: Value) -> Result<Value, String> {
    let arithmetic = |result: Option<u64>, what: &str| {
        result
            .map(Value::U64)
            .ok_or_else(|| format!("u64 {what} in '{}'", op.symbol()))
    };
    Ok(match op {
        BinaryOp::Add => arithmetic(left.as_u64().checked_add(right.as_u64()), "overflow")?,
        BinaryOp::Subtract => arithmetic(left.as_u64().checked_sub(right.as_u64()), "underflow")?,
        BinaryOp::Multiply => arithmetic(left.as_u64().checked_mul(right.as_u64()), "overflow")?,
        BinaryOp::Concat => Value::Text(format!("{}{}", left.as_text(), right.as_text())),
        BinaryOp::Equal => Value::Bool(left == right),
        BinaryOp::NotEqual => Value::Bool(left != right),
        BinaryOp::Less => Value::Bool(left.as_u64() < right.as_u64()),
        BinaryOp::LessEqual => Value::Bool(left.as_u64() <= right.as_u64()),
        BinaryOp::Greater => Value::Bool(left.as_u64() > right.as_u64()),
        BinaryOp::GreaterEqual => Value::Bool(left.as_u64() >= right.as_u64()),
        BinaryOp::And => Value::Bool(left.as_bool() & right.as_bool()),
        BinaryOp::Or => Value::Bool(left.as_bool() | right.as_bool()),
    })
}
