use std::collections::BTreeMap;

use crate::effect::EffectClass;
use crate::ir::{self, BinaryOp, Expr, Handler, Program, Site, SiteClass, SiteKind, Stmt, Type};
use crate::run::cell::{self, CellEvent, CellHost, CellOutcome, CellPoint, Input, Step, Wait};
use crate::run::cell_journal::{CellLog, divergence_text};
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NestedRef {
    pub id: u64,
    pub record: u64,
    pub cell: u64,
    pub code: crate::ir::CodeId,
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
}

enum Flow {
    Next,
    Return(Value),
}

pub struct Ran {
    pub finish: Finish,
    pub state: Vec<Value>,
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
        }
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
        let payload = if self.mutation == Mutation::CheckpointHashOrder {
            value::state_text_hashed(self.program, &self.state)
        } else {
            value::state_json(self.program, &self.state).encode()
        };
        self.append(Record::Checkpoint {
            revision: self.turn,
            state: self.program.name.clone(),
            payload,
        });
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
                for iteration in 0..bound.value {
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
            SiteKind::Dispatch => {
                let (_, fields) = args[0].as_variant();
                let tool = fields[ir::CALL_TOOL].as_text().to_owned();
                let effect = self.program.dispatch_class(&tool);
                Call {
                    tool,
                    effect,
                    input: Json::String(fields[ir::CALL_INPUT].as_text().to_owned()),
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
        }
    }

    fn effect(&mut self, site_id: usize, args: Vec<Value>) -> Result<Value, Halt> {
        let site = &self.handler.sites[site_id];
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
        if let SiteClass::Fixed(class) = site.class {
            assert_eq!(
                class, call.effect,
                "a fixed site runs with its declared class"
            );
        }
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &recorded, &call)?;
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
            Some(SiteLog::Reply(_)) => return Err(self.diverged(seq, "a model reply", &call.tool)),
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
            Mode::Replay => unreachable!("cells run only live"),
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
            SiteKind::Cell(_) => unreachable!("a cell site runs through the cell runner"),
            SiteKind::Tool(_) | SiteKind::Dispatch => {
                let returns = match &site.result {
                    Type::Known(inner) | Type::Maybe(inner) => (**inner).clone(),
                    other => panic!("a tool site yields known or maybe, found {other:?}"),
                };
                let output = world.tool(&ToolRequest {
                    turn: self.turn,
                    seq,
                    tool: &call.tool,
                    effect: call.effect,
                    input: &call.input,
                    returns: &returns,
                    nested: None,
                });
                if self.mutation == Mutation::EnumUnchecked {
                    value::fallback_output(self.program, &returns, output)
                } else {
                    value::checked_output(self.program, &call.tool, &returns, output)
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
        self.decode(&site, &output)
    }

    fn reply(&self, response: &Json) -> Result<Value, Halt> {
        value::from_json(self.program, &Type::Sum(ir::REPLY), response)
            .map_err(|error| Halt::Trap(format!("model reply: {error}")))
    }

    fn decode(&self, site: &Site, output: &Output) -> Result<Value, Halt> {
        let corrupt = |error: String| {
            Halt::Exit(Exit::Corrupt(format!("turn {} result: {error}", self.turn)))
        };
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
