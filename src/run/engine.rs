use std::collections::BTreeMap;

use crate::effect::EffectClass;
use crate::ir::{
    self, BinaryOp, Expr, Handler, Kept, Parallel, Program, Site, SiteClass, SiteKind, Stmt, Type,
};
use crate::run::journal::{Outcome, Output, Record, call_id};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::session::{Call, SiteLog, TurnLog};
use crate::run::value::{self, Value};
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
    pub admitted: Option<&'a str>,
}

pub struct ListingRequest<'a> {
    pub turn: u64,
    pub seq: u64,
    pub listing: &'a str,
}

pub struct ModelRequest<'a> {
    pub turn: u64,
    pub seq: u64,
    pub calls_max: u64,
    pub context: &'a Value,
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
    retired: u64,
    unretired: BTreeMap<usize, Vec<Value>>,
    admitted: BTreeMap<ir::ListingId, BTreeMap<String, String>>,
}

enum Flow {
    Next,
    Return(Value),
}

pub struct Ran {
    pub finish: Finish,
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
            retired: 0,
            unretired: BTreeMap::new(),
            admitted: BTreeMap::new(),
        }
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
        let finish = match flow {
            Ok(Flow::Return(value)) => self.finish(
                Outcome::Completed,
                value::to_json(self.program, &self.handler.returns, &value),
            ),
            Ok(Flow::Next) => panic!("the checker proves every handler returns"),
            Err(Halt::Trap(trap)) => self.finish(
                Outcome::Failed,
                Json::object([("trap", Json::String(trap))]),
            ),
            Err(Halt::Exit(exit)) => Err(exit),
        }?;
        Ok(Ran {
            finish,
            state: self.state,
            retired: self.retired,
        })
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
        self.flush().map_err(halt_exit)?;
        assert!(
            self.buffer.is_empty(),
            "a turn's status is durable before it is acknowledged"
        );
        self.hook(Point::BeforeAcknowledge).map_err(halt_exit)?;
        self.acknowledge(&finish);
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
        let Mode::Live { world, epoch } = &mut self.mode else {
            return Ok(());
        };
        if self.buffer.is_empty() {
            return Ok(());
        }
        world.hook(Point::BeforeFlush)?;
        world.flush(*epoch, &self.buffer)?;
        self.buffer.clear();
        world.hook(Point::AfterFlush)?;
        Ok(())
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
                values[index] = Some(self.decode(site, &output)?);
                Ok(None)
            }
            Some(SiteLog::Reply(_)) => Err(self.diverged(seq, "a model reply", &call.tool)),
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
            values[waiting.index] = Some(self.decode(&site, &output)?);
        }
        if late || in_step {
            self.flush()?;
        }
        Ok(())
    }

    fn batch_dispatch(&mut self, group: &[Pending]) -> Result<Vec<(usize, Output)>, Halt> {
        let returns = Type::Text;
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
                    returns: &returns,
                    admitted: gates[*position].as_ref().ok().and_then(Option::as_deref),
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
                let output = if self.mutation == Mutation::EnumUnchecked {
                    value::fallback_output(self.program, &returns, output)
                } else {
                    value::checked_output(self.program, tool, &returns, output)
                };
                (position, output)
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
        let count = match kept {
            Kept::Nothing => 0,
            Kept::Last(count) => self.eval(count)?.as_u64(),
        };
        let Value::List(items) = &mut self.state[field] else {
            panic!("the checker clears and keeps only lists");
        };
        let length = items.len() as u64;
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

    fn eval(&self, expr: &Expr) -> Result<Value, Halt> {
        evaluate(self.program, &self.state, &self.locals, expr).map_err(Halt::Trap)
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
        }
    }

    fn effect(&mut self, site_id: usize, args: Vec<Value>) -> Result<Value, Halt> {
        let site = &self.handler.sites[site_id];
        self.seq += 1;
        let seq = self.seq;
        let call = self.resolve(site, &args);
        if let SiteClass::Fixed(class) = site.class {
            assert_eq!(
                class, call.effect,
                "a fixed site runs with its declared class"
            );
        }
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &recorded, &call)?;
                self.admit_listing(site, &output)?;
                self.decode(site, &output)
            }
            Some(SiteLog::Reply(response)) => {
                if site.kind != SiteKind::Model {
                    return Err(self.diverged(seq, "a model reply", &call.tool));
                }
                self.reply(&response)
            }
            Some(SiteLog::Unknown(recorded)) => {
                self.same(seq, &recorded, &call)?;
                if !matches!(site.result, Type::Maybe(_)) {
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
                let response = world.model(&ModelRequest {
                    turn: self.turn,
                    seq,
                    calls_max,
                    context: &args[0],
                });
                self.hook(Point::AfterRun(site.kind, call.effect))?;
                if self.reply_unrecorded(&response) {
                    return self.reply(&response);
                }
                self.append(Record::Model {
                    turn: self.turn,
                    seq,
                    response: response.clone(),
                });
                self.sites.insert(seq, SiteLog::Reply(response.clone()));
                return self.reply(&response);
            }
            SiteKind::Now => Output::Ok(Json::Number(world.now())),
            SiteKind::Random => Output::Ok(Json::Number(world.random())),
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
                    other => panic!("a tool site yields known or maybe, found {other:?}"),
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
                        });
                        if self.mutation == Mutation::EnumUnchecked {
                            value::fallback_output(self.program, &returns, output)
                        } else {
                            value::checked_output(self.program, &call.tool, &returns, output)
                        }
                    }
                }
            }
        };
        self.hook(Point::AfterRun(site.kind, call.effect))?;
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
        self.admit_listing(&site, &output)?;
        self.decode(&site, &output)
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
        match self
            .admitted
            .get(&listing)
            .and_then(|tools| tools.get(name))
        {
            Some(fingerprint) => Ok(Some(fingerprint.clone())),
            None => Err(value::not_admitted(
                &call.tool,
                &self.program.listings[listing].name,
                name,
            )),
        }
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
                self.admitted.insert(listing, tools);
            }
            Output::Failed(_) => {
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
        if model.declared() {
            return Ok(value::model_outcome(self.program, response));
        }
        value::from_json(self.program, &Type::Sum(ir::REPLY), response)
            .map_err(|error| Halt::Trap(format!("model reply: {error}")))
    }

    fn reply_unrecorded(&self, response: &Json) -> bool {
        self.mutation == Mutation::ModelFailureUnrecorded
            && self.program.model.as_ref().is_some_and(ir::Model::declared)
            && value::model_failure(response).is_some()
    }

    fn decode(&self, site: &Site, output: &Output) -> Result<Value, Halt> {
        let corrupt = |error: String| {
            Halt::Exit(Exit::Corrupt(format!("turn {} result: {error}", self.turn)))
        };
        if let (SiteKind::Listing(_), Output::Ok(json)) = (site.kind, output) {
            let tools = value::admitted(json).map_err(corrupt)?;
            return Ok(Value::Variant(ir::OK, vec![Value::U64(tools.len() as u64)]));
        }
        match (&site.result, output) {
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

fn halt_exit(halt: Halt) -> Exit {
    match halt {
        Halt::Exit(exit) => exit,
        Halt::Trap(trap) => panic!("finishing a turn cannot trap, found {trap}"),
    }
}

pub fn evaluate(
    program: &Program,
    state: &[Value],
    locals: &[Option<Value>],
    expr: &Expr,
) -> Result<Value, String> {
    let eval = |expr: &Expr| evaluate(program, state, locals, expr);
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
