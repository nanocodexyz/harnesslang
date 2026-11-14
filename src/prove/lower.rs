use std::collections::BTreeSet;

use crate::effect::EffectClass;
use crate::ir::{
    BinaryOp, CodeId, Expr, Handler, Program, SiteClass, SiteId, SiteKind, Stmt, ToolId, Type,
};
use crate::prove::Mutation;
use crate::prove::text::Names;
use crate::run::mutation::Mutation as RunMutation;
use crate::target::{self, AtomicUnit, Flush, Recovery};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    Site(SiteId),
    Nested(SiteId, ToolId),
}

impl Slot {
    pub const fn site(self) -> SiteId {
        match self {
            Self::Site(site) | Self::Nested(site, _) => site,
        }
    }

    pub const fn nested(self) -> bool {
        matches!(self, Self::Nested(..))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Record {
    Admitted,
    Recovery,
    Call(Slot),
    Result(Slot),
    Model(SiteId),
    Status,
    Checkpoint,
    CellHeader(SiteId),
    Timeout(SiteId),
    CellFinished(SiteId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    BeforeExternal,
    OutputGate,
    StepCommit,
    Recovery,
    Finish,
    CellWait,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    Unknown,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Plain,
    Live,
    Rerun,
    Replayed,
    Reopened(Slot),
    FirstAttempt(Slot),
    LaterAttempt(Slot),
    Recorded(Slot),
    CellRecorded(SiteId),
    CellResumed(SiteId),
    CellRunning(SiteId),
    CellStopped(SiteId),
    Diverged(SiteId),
    HeaderPending(SiteId),
    HeaderDurable(SiteId),
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub label: String,
    pub entry: Entry,
    pub ops: Vec<Op>,
}

#[derive(Clone, Debug)]
pub enum Op {
    Append(Record),
    Flush(Point),
    FlushExcept(Point, Record),
    Run {
        slot: Slot,
        class: EffectClass,
        outbound: bool,
        what: String,
    },
    Yield {
        slot: Slot,
        class: EffectClass,
        seen: Seen,
        what: String,
    },
    Replay(Slot),
    Wait {
        site: SiteId,
        what: String,
    },
    CloseCell(SiteId, CodeId),
    Acknowledge,
    Choice(Vec<Arm>),
    Repeat {
        label: String,
        bound: u64,
        every_iteration: bool,
        body: Vec<Op>,
    },
    End,
}

#[derive(Clone, Debug)]
pub struct HandlerPlan {
    pub name: String,
    pub sites: Vec<String>,
    pub ops: Vec<Op>,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub model: target::Model,
    pub tools: Vec<String>,
    pub fenced: bool,
    pub checksummed: bool,
    pub checkpoint_sorted: bool,
    pub cells_fenced: bool,
    pub nested_ids_continue: bool,
    pub cell_limits_checked: bool,
    pub handlers: Vec<HandlerPlan>,
}

pub fn lower(program: &Program, model: &target::Model, mutation: Mutation) -> Plan {
    let run = mutation.lowering();
    let handlers = program
        .handlers
        .iter()
        .map(|handler| {
            let lowering = Lowering {
                program,
                handler,
                model,
                mutation,
                names: Names::new(program, Some(handler)),
            };
            HandlerPlan {
                name: handler.name.clone(),
                sites: lowering.site_names(),
                ops: lowering.handler(),
            }
        })
        .collect();
    Plan {
        model: *model,
        tools: program.tools.iter().map(|tool| tool.name.clone()).collect(),
        fenced: model.flush == Flush::FencedTransaction && run != RunMutation::PostgresNoFence,
        checksummed: model.atomic == AtomicUnit::ChecksummedRecord
            && run != RunMutation::JsonlNoChecksum,
        checkpoint_sorted: run != RunMutation::CheckpointHashOrder,
        cells_fenced: model.flush == Flush::FencedTransaction
            && run != RunMutation::PostgresCellUnfenced,
        nested_ids_continue: run != RunMutation::NestedIdsRestart,
        cell_limits_checked: run != RunMutation::CellLimitUnchecked,
        handlers,
    }
}

fn arm(label: String, entry: Entry, ops: Vec<Op>) -> Arm {
    Arm { label, entry, ops }
}

fn arithmetic(value: &Expr) -> bool {
    match value {
        Expr::Binary {
            op, left, right, ..
        } => {
            matches!(op, BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply)
                || arithmetic(left)
                || arithmetic(right)
        }
        Expr::Not(operand) | Expr::Len(operand) => arithmetic(operand),
        Expr::Member { base, .. } => arithmetic(base),
        Expr::Construct { fields, .. } => fields.iter().any(arithmetic),
        Expr::U64(_)
        | Expr::Bool(_)
        | Expr::Text(_)
        | Expr::Constant(_)
        | Expr::Local(_)
        | Expr::Field(_) => false,
    }
}

struct Lowering<'a> {
    program: &'a Program,
    handler: &'a Handler,
    model: &'a target::Model,
    mutation: Mutation,
    names: Names<'a>,
}

impl Lowering<'_> {
    fn run_mutation(&self) -> RunMutation {
        self.mutation.lowering()
    }

    fn in_step(&self) -> bool {
        matches!(self.model.recovery, Recovery::InStep { .. })
    }

    fn site_names(&self) -> Vec<String> {
        let mut names = vec![String::new(); self.handler.sites.len()];
        self.collect_names(&self.handler.body, &mut names);
        names
    }

    fn collect_names(&self, statements: &[Stmt], names: &mut [String]) {
        for statement in statements {
            match statement {
                Stmt::Effect { site, args, .. } => names[*site] = self.names.site(*site, args),
                Stmt::If {
                    then, otherwise, ..
                } => {
                    self.collect_names(then, names);
                    self.collect_names(otherwise, names);
                }
                Stmt::Match { arms, .. } => arms
                    .iter()
                    .for_each(|arm| self.collect_names(&arm.body, names)),
                Stmt::Loop { body, .. } | Stmt::For { body, .. } => {
                    self.collect_names(body, names);
                }
                Stmt::Let { .. }
                | Stmt::Assign { .. }
                | Stmt::Push { .. }
                | Stmt::Finish { .. } => {}
            }
        }
    }

    fn handler(&self) -> Vec<Op> {
        let skipped = self.run_mutation() == RunMutation::RecoverySkipped;
        let rerun = match self.model.recovery {
            Recovery::AtLoad if !skipped => vec![Op::Append(Record::Recovery)],
            Recovery::AtLoad | Recovery::InStep { .. } => Vec::new(),
        };
        let mut ops = vec![Op::Choice(vec![
            arm(
                "first run".to_owned(),
                Entry::Live,
                vec![Op::Append(Record::Admitted)],
            ),
            arm("rerun after a crash".to_owned(), Entry::Rerun, rerun),
        ])];
        if let Some(requires) = &self.handler.requires {
            let text = self.names.expr(requires);
            ops.push(Op::Choice(vec![
                arm(format!("requires {text} holds"), Entry::Plain, Vec::new()),
                arm(
                    format!("requires {text} fails"),
                    Entry::Plain,
                    self.finish(),
                ),
            ]));
        }
        ops.extend(self.block(&self.handler.body));
        ops
    }

    fn acknowledge(&self, ops: &mut Vec<Op>) {
        if self.model.gate_before_outbound {
            ops.push(Op::Flush(Point::OutputGate));
        }
        ops.push(Op::Acknowledge);
    }

    fn finish(&self) -> Vec<Op> {
        let mut ops = vec![Op::Append(Record::Status), Op::Append(Record::Checkpoint)];
        if self.run_mutation() == RunMutation::FinishBeforeFlush {
            self.acknowledge(&mut ops);
            ops.push(Op::Flush(Point::Finish));
        } else {
            ops.push(Op::Flush(Point::Finish));
            self.acknowledge(&mut ops);
        }
        ops.push(Op::End);
        ops
    }

    fn trap(&self, what: String) -> Op {
        Op::Choice(vec![
            arm(format!("{what} does not trap"), Entry::Plain, Vec::new()),
            arm(format!("{what} traps"), Entry::Plain, self.finish()),
        ])
    }

    fn arithmetic_trap(&self, value: &Expr, ops: &mut Vec<Op>) {
        if arithmetic(value) {
            ops.push(self.trap(format!("'{}'", self.names.expr(value))));
        }
    }

    fn block(&self, statements: &[Stmt]) -> Vec<Op> {
        let mut ops = Vec::new();
        for statement in statements {
            self.statement(statement, &mut ops);
        }
        ops
    }

    fn statement(&self, statement: &Stmt, ops: &mut Vec<Op>) {
        match statement {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } => {
                self.arithmetic_trap(value, ops);
            }
            Stmt::Push { field, value } => {
                self.arithmetic_trap(value, ops);
                let name = &self.program.state[*field].name;
                ops.push(self.trap(format!("push onto {name}")));
            }
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                self.arithmetic_trap(condition, ops);
                let text = self.names.expr(condition);
                ops.push(Op::Choice(vec![
                    arm(format!("if {text}"), Entry::Plain, self.block(then)),
                    arm(
                        format!("else of if {text}"),
                        Entry::Plain,
                        self.block(otherwise),
                    ),
                ]));
            }
            Stmt::Match { subject, ty, arms } => {
                self.arithmetic_trap(subject, ops);
                let text = self.names.expr(subject);
                let variants = ty.variants(self.program).expect("a match subject is a sum");
                let arms = arms
                    .iter()
                    .map(|choice| {
                        let label = format!("match {text}: {}", variants[choice.variant].name);
                        arm(label, Entry::Plain, self.block(&choice.body))
                    })
                    .collect();
                ops.push(Op::Choice(arms));
            }
            Stmt::Loop { index, bound, body } => ops.push(Op::Repeat {
                label: format!(
                    "loop {} below {} ({})",
                    self.handler.locals[*index].name, bound.name, bound.value
                ),
                bound: bound.value,
                every_iteration: true,
                body: self.block(body),
            }),
            Stmt::For {
                item,
                list,
                bound,
                body,
            } => {
                self.arithmetic_trap(list, ops);
                ops.push(Op::Repeat {
                    label: format!(
                        "for {} in {} (at most {} = {})",
                        self.handler.locals[*item].name,
                        self.names.expr(list),
                        bound.name,
                        bound.value
                    ),
                    bound: bound.value,
                    every_iteration: false,
                    body: self.block(body),
                });
            }
            Stmt::Effect { site, args, .. } => {
                args.iter().for_each(|arg| self.arithmetic_trap(arg, ops));
                ops.extend(self.effect(*site, args));
                if self.handler.sites[*site].kind == SiteKind::Model {
                    ops.push(self.trap(format!(
                        "decoding the reply of {}",
                        self.names.site(*site, args)
                    )));
                }
            }
            Stmt::Finish { value } => {
                self.arithmetic_trap(value, ops);
                ops.extend(self.finish());
            }
        }
    }

    fn dispatch_classes(&self) -> Vec<EffectClass> {
        let mut classes = BTreeSet::from([EffectClass::undeclared()]);
        classes.extend(self.program.tools.iter().map(|tool| tool.effect));
        classes.into_iter().collect()
    }

    fn effect(&self, site: SiteId, args: &[Expr]) -> Vec<Op> {
        let what = self.names.site(site, args);
        let slot = Slot::Site(site);
        let classes = match self.handler.sites[site].class {
            SiteClass::Fixed(class) => vec![class],
            SiteClass::Dispatched => self.dispatch_classes(),
            SiteClass::Cell => {
                let SiteKind::Cell(code) = self.handler.sites[site].kind else {
                    unreachable!("a site of the cell class is a cell site")
                };
                return self.cell(site, code, &what);
            }
        };
        if let [class] = classes.as_slice() {
            return self.resolved(slot, *class, &what);
        }
        let mut arms: Vec<Arm> = classes
            .into_iter()
            .map(|class| {
                let label = format!("{what} resolves to a tool with effect {}", class.name());
                arm(label, Entry::Plain, self.resolved(slot, class, &what))
            })
            .collect();
        for (code, declared) in self.program.codes.iter().enumerate() {
            let label = format!("{what} resolves to code tool {}", declared.name);
            arms.push(arm(label, Entry::Plain, self.cell(site, code, &what)));
        }
        vec![Op::Choice(arms)]
    }

    fn resolved(&self, slot: Slot, class: EffectClass, what: &str) -> Vec<Op> {
        let fresh = self.fresh(slot, class, what);
        let reopenable = self.reopenable(slot, &fresh);
        let mut arms = vec![arm(format!("{what} runs"), Entry::Live, fresh)];
        if reopenable {
            arms.push(arm(
                format!("{what} reopened after a crash left its call open"),
                Entry::Reopened(slot),
                self.reopen(slot, class, what),
            ));
        }
        arms.push(arm(
            format!("{what} replayed from its recorded result"),
            Entry::Replayed,
            Vec::new(),
        ));
        vec![Op::Choice(arms)]
    }

    fn is_model(&self, slot: Slot) -> bool {
        matches!(slot, Slot::Site(site) if self.handler.sites[site].kind == SiteKind::Model)
    }

    fn outbound(&self, slot: Slot) -> bool {
        match slot {
            Slot::Site(site) => self.handler.sites[site].kind.outbound(),
            Slot::Nested(..) => true,
        }
    }

    fn reopenable(&self, slot: Slot, fresh: &[Op]) -> bool {
        if self.is_model(slot) {
            return false;
        }
        if slot.nested() || self.model.atomic == AtomicUnit::ChecksummedRecord {
            return true;
        }
        let mut called = false;
        for op in fresh {
            match op {
                Op::Append(Record::Call(_)) => called = true,
                Op::Flush(_) if called => return true,
                Op::Run { .. } | Op::Choice(_) => return false,
                _ => {}
            }
        }
        false
    }

    fn gated(&self, slot: Slot) -> bool {
        self.model.gate_before_outbound
            && self.outbound(slot)
            && self.mutation != Mutation::DropFlush
    }

    fn required(&self, slot: Slot, class: EffectClass) -> bool {
        self.handler.sites[slot.site()].durability.flushes(class)
            && self.mutation != Mutation::DropFlush
    }

    fn run(&self, slot: Slot, class: EffectClass, what: &str, step: bool) -> Vec<Op> {
        let record = if self.is_model(slot) {
            Record::Model(slot.site())
        } else {
            Record::Result(slot)
        };
        let mut ops = vec![
            Op::Run {
                slot,
                class,
                outbound: self.outbound(slot),
                what: what.to_owned(),
            },
            Op::Append(record),
        ];
        if step {
            ops.push(Op::Flush(Point::StepCommit));
        }
        ops
    }

    fn late(&self, slot: Slot, required: bool, gate: bool) -> bool {
        match (self.run_mutation(), slot.nested()) {
            (RunMutation::FlushAfterDispatch, false)
            | (RunMutation::NestedFlushAfterDispatch, true) => required && !gate,
            (RunMutation::GateReleasesEarly, false)
            | (RunMutation::CellGateReleasesEarly, true) => gate,
            _ => false,
        }
    }

    fn before_run(&self, slot: Slot, point: Point) -> Op {
        let rides = self.run_mutation() == RunMutation::CellHeaderRidesPastExternal
            && slot.nested()
            && point == Point::BeforeExternal;
        if rides {
            Op::FlushExcept(point, Record::CellHeader(slot.site()))
        } else {
            Op::Flush(point)
        }
    }

    fn fresh(&self, slot: Slot, class: EffectClass, what: &str) -> Vec<Op> {
        let gate = self.gated(slot);
        let required = self.required(slot, class);
        let late = self.late(slot, required, gate);
        let step = self.in_step() && class.may_be_unknown();
        let point = if gate {
            Point::OutputGate
        } else {
            Point::BeforeExternal
        };
        let mut ops = Vec::new();
        if !self.is_model(slot) {
            ops.push(Op::Append(Record::Call(slot)));
        }
        if (required || gate) && !late {
            ops.push(self.before_run(slot, point));
        }
        let mut run = self.run(slot, class, what, step);
        if late {
            run.push(Op::Flush(point));
        }
        if step {
            ops.push(Op::Choice(vec![arm(
                format!("{what} reads attempt 1"),
                Entry::FirstAttempt(slot),
                run,
            )]));
        } else {
            ops.extend(run);
        }
        ops
    }

    fn rerun(&self, slot: Slot, class: EffectClass, what: &str) -> Vec<Op> {
        let mut ops = Vec::new();
        if self.required(slot, class) || self.gated(slot) {
            let point = if self.gated(slot) {
                Point::OutputGate
            } else {
                Point::BeforeExternal
            };
            ops.push(Op::Flush(point));
        }
        ops.extend(self.run(slot, class, what, false));
        ops
    }

    fn mutated(&self, slot: Slot, top: RunMutation, nested: RunMutation) -> bool {
        let wanted = if slot.nested() { nested } else { top };
        wanted != RunMutation::None && self.run_mutation() == wanted
    }

    fn reopen_in_step(&self, slot: Slot, class: EffectClass, what: &str) -> Vec<Op> {
        let redispatches = self.mutated(
            slot,
            RunMutation::VercelRetryRedispatches,
            RunMutation::VercelNestedRetryRedispatches,
        );
        let retry = if redispatches {
            self.run(slot, class, what, true)
        } else {
            vec![
                Op::Append(Record::Recovery),
                Op::Flush(Point::Recovery),
                Op::Yield {
                    slot,
                    class,
                    seen: Seen::Unknown,
                    what: what.to_owned(),
                },
            ]
        };
        let mut ops = Vec::new();
        if self.required(slot, class) {
            ops.push(self.before_run(slot, Point::BeforeExternal));
        }
        ops.push(Op::Choice(vec![
            arm(
                format!("{what} reads attempt 1: the crash came before its dispatch"),
                Entry::FirstAttempt(slot),
                self.run(slot, class, what, true),
            ),
            arm(
                format!("{what} reads attempt 2 or later"),
                Entry::LaterAttempt(slot),
                retry,
            ),
        ]));
        ops
    }

    fn reopen(&self, slot: Slot, class: EffectClass, what: &str) -> Vec<Op> {
        let unknown = |seen| Op::Yield {
            slot,
            class,
            seen,
            what: what.to_owned(),
        };
        if self.in_step() && class.may_be_unknown() {
            return self.reopen_in_step(slot, class, what);
        }
        if class.may_be_unknown() {
            let redispatches = self.run_mutation() == RunMutation::RecoverySkipped
                || self.mutated(
                    slot,
                    RunMutation::RecoveryRedispatchesExternal,
                    RunMutation::CellRecoveryRedispatchesExternal,
                );
            if redispatches {
                return self.rerun(slot, class, what);
            }
            let failed = self.mutated(
                slot,
                RunMutation::RecoveryReportsFailed,
                RunMutation::CellRecoveryReportsFailed,
            );
            return vec![unknown(if failed { Seen::Failed } else { Seen::Unknown })];
        }
        let maybe =
            slot.nested() || matches!(self.handler.sites[slot.site()].result, Type::Maybe(_));
        let reported = self.mutated(
            slot,
            RunMutation::IdempotentReportedUnknown,
            RunMutation::NestedIdempotentReportedUnknown,
        );
        if reported && maybe {
            return vec![unknown(Seen::Unknown)];
        }
        let mut ops = Vec::new();
        if self.mutated(slot, RunMutation::RerunRewritesCall, RunMutation::None) {
            ops.push(Op::Append(Record::Call(slot)));
        }
        ops.extend(self.rerun(slot, class, what));
        ops
    }

    fn cell(&self, site: SiteId, code: CodeId, what: &str) -> Vec<Op> {
        let body = self.cell_body(site, code, what);
        let mut restarted = vec![Op::Append(Record::CellHeader(site))];
        restarted.extend(body.iter().cloned());
        let mut fresh = vec![Op::Append(Record::Call(Slot::Site(site)))];
        fresh.extend(restarted.iter().cloned());
        let recorded = if self.run_mutation() == RunMutation::CellRerunAfterFinish {
            restarted.clone()
        } else {
            Vec::new()
        };
        vec![
            Op::Choice(vec![
                arm(format!("{what} starts a cell"), Entry::Live, fresh),
                arm(
                    format!("{what} reopened without a durable header: its cell starts again"),
                    Entry::Reopened(Slot::Site(site)),
                    restarted,
                ),
                arm(
                    format!("{what} resumed from its durable header: its cell is replayed"),
                    Entry::CellResumed(site),
                    body,
                ),
                arm(
                    format!("{what} replayed from its recorded result"),
                    Entry::CellRecorded(site),
                    recorded,
                ),
            ]),
            Op::CloseCell(site, code),
        ]
    }

    fn cell_body(&self, site: SiteId, code: CodeId, what: &str) -> Vec<Op> {
        let declared = &self.program.codes[code];
        let waits = Op::Repeat {
            label: format!(
                "timers of {what} (at most {} = {})",
                declared.timers.name, declared.timers.value
            ),
            bound: declared.timers.value,
            every_iteration: false,
            body: vec![Op::Choice(vec![
                arm(format!("{what} arms no timer"), Entry::Plain, Vec::new()),
                arm(
                    format!("the cell of {what} waits on a timer"),
                    Entry::CellRunning(site),
                    self.wait(site, what),
                ),
            ])],
        };
        let step = vec![Op::Choice(self.cell_steps(site, code, what)), waits.clone()];
        let finished = if self.mutation == Mutation::DropCellFinish {
            Vec::new()
        } else {
            vec![Op::Append(Record::CellFinished(site))]
        };
        vec![
            waits,
            Op::Repeat {
                label: format!(
                    "nested calls of {what} (at most {} = {})",
                    declared.calls.name, declared.calls.value
                ),
                bound: declared.calls.value,
                every_iteration: false,
                body: step,
            },
            Op::Choice(vec![
                arm(
                    format!("the cell of {what} returns or throws"),
                    Entry::CellRunning(site),
                    finished,
                ),
                arm(
                    format!("the cell of {what} was stopped"),
                    Entry::CellStopped(site),
                    Vec::new(),
                ),
            ]),
            Op::Append(Record::Result(Slot::Site(site))),
        ]
    }

    fn wait(&self, site: SiteId, what: &str) -> Vec<Op> {
        let mut ops = vec![Op::Append(Record::Timeout(site))];
        if self.model.wait_flushes() {
            ops.push(Op::Flush(Point::OutputGate));
        } else {
            let saves = if self.run_mutation() == RunMutation::TimeoutRides {
                Vec::new()
            } else {
                vec![Op::Flush(Point::CellWait)]
            };
            ops.push(Op::Choice(vec![
                arm(
                    format!("the header of {what}'s cell is not yet durable"),
                    Entry::HeaderPending(site),
                    saves,
                ),
                arm(
                    format!("the header of {what}'s cell is durable"),
                    Entry::HeaderDurable(site),
                    Vec::new(),
                ),
            ]));
        }
        ops.push(Op::Wait {
            site,
            what: format!("the cell of {what}"),
        });
        ops
    }

    fn cell_steps(&self, site: SiteId, code: CodeId, what: &str) -> Vec<Arm> {
        let declared = &self.program.codes[code];
        let mut arms: Vec<Arm> = declared
            .tools
            .iter()
            .map(|tool| {
                let name = &self.program.tools[*tool].name;
                arm(
                    format!("the cell of {what} calls tools.{name}"),
                    Entry::CellRunning(site),
                    vec![Op::Choice(self.nested(site, *tool, what))],
                )
            })
            .collect();
        let mut diverged = Vec::new();
        if self.run_mutation() == RunMutation::DivergenceDispatches {
            let widest = self.widest_tool(code);
            diverged.extend(self.fresh(
                Slot::Nested(site, widest),
                self.program.tools[widest].effect,
                &self.nested_what(widest, what),
            ));
        }
        diverged.push(Op::Append(Record::CellFinished(site)));
        arms.push(arm(
            format!("the cell of {what} diverges from its journal on replay"),
            Entry::Diverged(site),
            diverged,
        ));
        arms.push(arm(
            format!(
                "the cell journal of {what} passes {} = {} bytes",
                declared.journal_bytes.name, declared.journal_bytes.value
            ),
            Entry::CellRunning(site),
            vec![Op::Append(Record::CellFinished(site))],
        ));
        arms
    }

    fn widest_tool(&self, code: CodeId) -> ToolId {
        let declared = &self.program.codes[code];
        let class = declared.worst_class(self.program);
        *declared
            .tools
            .iter()
            .find(|tool| self.program.tools[**tool].effect == class)
            .expect("the worst class is some nested tool's class")
    }

    fn nested_what(&self, tool: ToolId, what: &str) -> String {
        format!(
            "tools.{} in the cell of {what}",
            self.program.tools[tool].name
        )
    }

    fn nested(&self, site: SiteId, tool: ToolId, what: &str) -> Vec<Arm> {
        let slot = Slot::Nested(site, tool);
        let class = self.program.tools[tool].effect;
        let what = self.nested_what(tool, what);
        let reopened = self.yield_result(slot, self.reopen(slot, class, &what));
        let recorded = if self.run_mutation() == RunMutation::CellReplayIgnoresJournal {
            self.fresh(slot, class, &what)
        } else {
            vec![Op::Replay(slot)]
        };
        vec![
            arm(
                format!("{what} runs"),
                Entry::Live,
                self.fresh(slot, class, &what),
            ),
            arm(
                format!("{what} reopened after a crash left it running"),
                Entry::Reopened(slot),
                reopened,
            ),
            arm(
                format!("{what} replayed from its journaled result"),
                Entry::Recorded(slot),
                recorded,
            ),
        ]
    }

    fn yield_result(&self, slot: Slot, ops: Vec<Op>) -> Vec<Op> {
        ops.into_iter()
            .flat_map(|op| match op {
                Op::Yield { .. } => vec![op, Op::Append(Record::Result(slot))],
                Op::Choice(arms) => vec![Op::Choice(
                    arms.into_iter()
                        .map(|choice| Arm {
                            ops: self.yield_result(slot, choice.ops),
                            ..choice
                        })
                        .collect(),
                )],
                other => vec![other],
            })
            .collect()
    }
}
