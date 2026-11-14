use std::collections::BTreeSet;

use crate::effect::EffectClass;
use crate::ir::{BinaryOp, Expr, Handler, Parallel, Program, SiteId, SiteKind, Stmt, Type};
use crate::prove::Mutation;
use crate::prove::text::Names;
use crate::run::mutation::Mutation as RunMutation;
use crate::target::{self, AtomicUnit, Flush, Recovery};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Record {
    Admitted,
    Recovery,
    Call(SiteId),
    Result(SiteId),
    Model(SiteId),
    BatchCall(SiteId),
    BatchResult(SiteId),
    Status,
    Checkpoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    BeforeExternal,
    OutputGate,
    StepCommit,
    Recovery,
    Finish,
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
    Reopened(SiteId),
    ReopenedInBatch(SiteId),
    Has(SiteId),
    FirstAttempt(SiteId),
    LaterAttempt(SiteId),
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
    Run {
        site: SiteId,
        class: EffectClass,
        outbound: bool,
        what: String,
    },
    Yield {
        site: SiteId,
        class: EffectClass,
        seen: Seen,
        what: String,
    },
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
    pub real: Vec<SiteId>,
    pub ops: Vec<Op>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Batched {
    External,
    Other,
    Reopened,
}

const BATCHED: [Batched; 3] = [Batched::External, Batched::Other, Batched::Reopened];

impl Batched {
    const fn describe(self) -> &'static str {
        match self {
            Self::External => "its fresh external calls",
            Self::Other => "its none and idempotent calls",
            Self::Reopened => "its external calls reopened in a retried step",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub model: target::Model,
    pub fenced: bool,
    pub checksummed: bool,
    pub checkpoint_sorted: bool,
    pub handlers: Vec<HandlerPlan>,
}

pub fn lower(program: &Program, model: &target::Model, mutation: Mutation) -> Plan {
    let run = mutation.lowering();
    let handlers = program
        .handlers
        .iter()
        .map(|handler| {
            let mut batches = Vec::new();
            batch_sites(&handler.body, &mut batches);
            let lowering = Lowering {
                program,
                handler,
                model,
                mutation,
                names: Names::new(program, Some(handler)),
                batches,
            };
            let (sites, real) = lowering.site_names();
            HandlerPlan {
                name: handler.name.clone(),
                sites,
                real,
                ops: lowering.handler(),
            }
        })
        .collect();
    Plan {
        model: *model,
        fenced: model.flush == Flush::FencedTransaction && run != RunMutation::PostgresNoFence,
        checksummed: model.atomic == AtomicUnit::ChecksummedRecord
            && run != RunMutation::JsonlNoChecksum,
        checkpoint_sorted: run != RunMutation::CheckpointHashOrder,
        handlers,
    }
}

fn batch_sites(statements: &[Stmt], out: &mut Vec<SiteId>) {
    for statement in statements {
        match statement {
            Stmt::Parallel(parallel) => out.push(parallel.site),
            Stmt::If {
                then, otherwise, ..
            } => {
                batch_sites(then, out);
                batch_sites(otherwise, out);
            }
            Stmt::Match { arms, .. } => arms.iter().for_each(|arm| batch_sites(&arm.body, out)),
            Stmt::Loop { body, .. } | Stmt::For { body, .. } => batch_sites(body, out),
            Stmt::Let { .. }
            | Stmt::Assign { .. }
            | Stmt::Push { .. }
            | Stmt::Effect { .. }
            | Stmt::Finish { .. } => {}
        }
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
    batches: Vec<SiteId>,
}

impl Lowering<'_> {
    fn run_mutation(&self) -> RunMutation {
        self.mutation.lowering()
    }

    fn in_step(&self) -> bool {
        matches!(self.model.recovery, Recovery::InStep { .. })
    }

    fn site_names(&self) -> (Vec<String>, Vec<SiteId>) {
        let mut names = vec![String::new(); self.handler.sites.len()];
        self.collect_names(&self.handler.body, &mut names);
        let mut real: Vec<SiteId> = (0..names.len()).collect();
        for site in &self.batches {
            for batched in BATCHED {
                assert_eq!(
                    names.len(),
                    self.batched(*site, batched),
                    "a batch's virtual sites follow the handler's sites in order"
                );
                names.push(format!(
                    "{}, {} in its parallel for",
                    names[*site],
                    batched.describe()
                ));
                real.push(*site);
            }
        }
        (names, real)
    }

    fn batched(&self, site: SiteId, batched: Batched) -> SiteId {
        let rank = self
            .batches
            .iter()
            .position(|candidate| *candidate == site)
            .expect("a parallel for's site is a batch site");
        let offset = BATCHED
            .iter()
            .position(|candidate| *candidate == batched)
            .expect("every batched kind is listed");
        self.handler.sites.len() + rank * BATCHED.len() + offset
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
                Stmt::Parallel(parallel) => {
                    names[parallel.site] = self.names.site(parallel.site, &parallel.args);
                    self.collect_names(&parallel.body, names);
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
            Stmt::Parallel(parallel) => self.parallel(parallel, ops),
            Stmt::Effect { site, args, .. } => {
                args.iter().for_each(|arg| self.arithmetic_trap(arg, ops));
                ops.extend(self.effect(*site, args));
                if self.handler.sites[*site].traps_on_decode() {
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

    fn parallel(&self, parallel: &Parallel, ops: &mut Vec<Op>) {
        self.arithmetic_trap(&parallel.list, ops);
        let label = format!(
            "parallel for {} in {} (at most {} = {})",
            self.handler.locals[parallel.item].name,
            self.names.expr(&parallel.list),
            parallel.bound.name,
            parallel.bound.value
        );
        let what = self.names.site(parallel.site, &parallel.args);
        let calls = self
            .dispatch_classes()
            .into_iter()
            .map(|class| {
                let label = format!("{what} resolves to a tool with effect {}", class.name());
                arm(
                    label,
                    Entry::Plain,
                    self.batch_call(parallel.site, class, &what),
                )
            })
            .collect();
        ops.push(Op::Repeat {
            label: format!("{label}: the call records"),
            bound: parallel.bound.value,
            every_iteration: false,
            body: vec![Op::Choice(calls)],
        });
        ops.push(Op::Repeat {
            label: format!("{label}: the dispatches"),
            bound: parallel.bound.value,
            every_iteration: false,
            body: vec![Op::Choice(self.batch_runs(parallel.site, &what))],
        });
        ops.push(Op::Repeat {
            label: format!("{label}: the pushes in list order"),
            bound: parallel.bound.value,
            every_iteration: false,
            body: self.block(&parallel.body),
        });
    }

    fn batch_call(&self, site: SiteId, class: EffectClass, what: &str) -> Vec<Op> {
        let kind = if class.may_be_unknown() {
            Batched::External
        } else {
            Batched::Other
        };
        let mut arms = vec![arm(
            format!("{what} appends its call record before any dispatch"),
            Entry::Live,
            vec![Op::Append(Record::BatchCall(self.batched(site, kind)))],
        )];
        let reopened = format!("{what} reopened after a crash left its call open");
        if !class.may_be_unknown() {
            arms.push(arm(
                reopened,
                Entry::ReopenedInBatch(self.batched(site, Batched::Other)),
                Vec::new(),
            ));
        } else if self.in_step() {
            arms.push(arm(
                reopened,
                Entry::ReopenedInBatch(self.batched(site, Batched::Reopened)),
                Vec::new(),
            ));
        } else {
            arms.push(arm(
                format!("{what} reported unknown by recovery"),
                Entry::Replayed,
                vec![Op::Yield {
                    site,
                    class,
                    seen: Seen::Unknown,
                    what: what.to_owned(),
                }],
            ));
        }
        arms.push(arm(
            format!("{what} replayed from its recorded result"),
            Entry::Replayed,
            Vec::new(),
        ));
        vec![Op::Choice(arms)]
    }

    fn batch_runs(&self, site: SiteId, what: &str) -> Vec<Arm> {
        let gate = self.model.gate_before_outbound && self.mutation != Mutation::DropFlush;
        let other = self.batched(site, Batched::Other);
        let mut group = Vec::new();
        if gate {
            group.push(Op::Flush(Point::OutputGate));
        }
        group.push(Op::Run {
            site: other,
            class: EffectClass::None,
            outbound: true,
            what: format!("{what} in a group of none and idempotent calls"),
        });
        group.push(Op::Append(Record::BatchResult(other)));
        let mut arms = vec![arm(
            format!("a group of {what} with effect none or idempotent runs at once"),
            Entry::Has(other),
            group,
        )];
        let external = self.batched(site, Batched::External);
        let alone = self.batch_external(external, what, false);
        let alone = if self.in_step() {
            vec![Op::Choice(vec![arm(
                format!("external {what} reads attempt 1"),
                Entry::FirstAttempt(external),
                alone,
            )])]
        } else {
            alone
        };
        let mut external_ops = self.batch_flush(gate);
        external_ops.extend(alone);
        arms.push(arm(
            format!("external {what} runs alone"),
            Entry::Has(external),
            external_ops,
        ));
        if self.in_step() {
            let reopened = self.batched(site, Batched::Reopened);
            let mut ops = self.batch_flush(gate);
            ops.push(Op::Choice(vec![
                arm(
                    format!("reopened {what} reads attempt 1: the crash came before its dispatch"),
                    Entry::FirstAttempt(reopened),
                    self.batch_external(reopened, what, true),
                ),
                arm(
                    format!("reopened {what} reads attempt 2 or later"),
                    Entry::LaterAttempt(reopened),
                    vec![
                        Op::Append(Record::Recovery),
                        Op::Flush(Point::Recovery),
                        Op::Yield {
                            site: reopened,
                            class: EffectClass::External,
                            seen: Seen::Unknown,
                            what: what.to_owned(),
                        },
                    ],
                ),
            ]));
            arms.push(arm(
                format!("reopened external {what} runs alone"),
                Entry::Has(reopened),
                ops,
            ));
        }
        arms
    }

    fn batch_flush(&self, gate: bool) -> Vec<Op> {
        if self.mutation == Mutation::DropFlush {
            return Vec::new();
        }
        let point = if gate {
            Point::OutputGate
        } else {
            Point::BeforeExternal
        };
        vec![Op::Flush(point)]
    }

    fn batch_external(&self, site: SiteId, what: &str, reopened: bool) -> Vec<Op> {
        let label = if reopened { "reopened " } else { "" };
        let mut ops = vec![
            Op::Run {
                site,
                class: EffectClass::External,
                outbound: true,
                what: format!("{label}{what}"),
            },
            Op::Append(Record::BatchResult(site)),
        ];
        if self.in_step() {
            ops.push(Op::Flush(Point::StepCommit));
        }
        ops
    }

    fn dispatch_classes(&self) -> Vec<EffectClass> {
        let mut classes = BTreeSet::from([EffectClass::undeclared()]);
        classes.extend(self.program.tools.iter().map(|tool| tool.effect));
        classes.into_iter().collect()
    }

    fn effect(&self, site: SiteId, args: &[Expr]) -> Vec<Op> {
        let what = self.names.site(site, args);
        let classes = match self.handler.sites[site].class {
            crate::ir::SiteClass::Fixed(class) => vec![class],
            crate::ir::SiteClass::Dispatched => self.dispatch_classes(),
        };
        if let [class] = classes.as_slice() {
            return self.resolved(site, *class, &what);
        }
        let arms = classes
            .into_iter()
            .map(|class| {
                let label = format!("{what} resolves to a tool with effect {}", class.name());
                arm(label, Entry::Plain, self.resolved(site, class, &what))
            })
            .collect();
        vec![Op::Choice(arms)]
    }

    fn resolved(&self, site: SiteId, class: EffectClass, what: &str) -> Vec<Op> {
        let fresh = self.fresh(site, class, what);
        let reopenable = self.reopenable(site, &fresh);
        let mut arms = vec![arm(format!("{what} runs"), Entry::Live, fresh)];
        if reopenable {
            arms.push(arm(
                format!("{what} reopened after a crash left its call open"),
                Entry::Reopened(site),
                self.reopen(site, class, what),
            ));
        }
        arms.push(arm(
            format!("{what} replayed from its recorded result"),
            Entry::Replayed,
            Vec::new(),
        ));
        vec![Op::Choice(arms)]
    }

    fn reopenable(&self, site: SiteId, fresh: &[Op]) -> bool {
        if self.handler.sites[site].kind == SiteKind::Model {
            return false;
        }
        if self.model.atomic == AtomicUnit::ChecksummedRecord {
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

    fn gated(&self, site: SiteId) -> bool {
        self.model.gate_before_outbound
            && self.handler.sites[site].kind.outbound()
            && self.mutation != Mutation::DropFlush
    }

    fn required(&self, site: SiteId, class: EffectClass) -> bool {
        self.handler.sites[site].durability.flushes(class) && self.mutation != Mutation::DropFlush
    }

    fn run(&self, site: SiteId, class: EffectClass, what: &str, step: bool) -> Vec<Op> {
        let record = if self.handler.sites[site].kind == SiteKind::Model {
            Record::Model(site)
        } else {
            Record::Result(site)
        };
        let mut ops = vec![
            Op::Run {
                site,
                class,
                outbound: self.handler.sites[site].kind.outbound(),
                what: what.to_owned(),
            },
            Op::Append(record),
        ];
        if step {
            ops.push(Op::Flush(Point::StepCommit));
        }
        ops
    }

    fn fresh(&self, site: SiteId, class: EffectClass, what: &str) -> Vec<Op> {
        let gate = self.gated(site);
        let required = self.required(site, class);
        let late = match self.run_mutation() {
            RunMutation::FlushAfterDispatch => required && !gate,
            RunMutation::GateReleasesEarly => gate,
            _ => false,
        };
        let step = self.in_step() && class.may_be_unknown();
        let point = if gate {
            Point::OutputGate
        } else {
            Point::BeforeExternal
        };
        let mut ops = Vec::new();
        if self.handler.sites[site].kind != SiteKind::Model {
            ops.push(Op::Append(Record::Call(site)));
        }
        if (required || gate) && !late {
            ops.push(Op::Flush(point));
        }
        let mut run = self.run(site, class, what, step);
        if late {
            run.push(Op::Flush(point));
        }
        if step {
            ops.push(Op::Choice(vec![arm(
                format!("{what} reads attempt 1"),
                Entry::FirstAttempt(site),
                run,
            )]));
        } else {
            ops.extend(run);
        }
        ops
    }

    fn rerun(&self, site: SiteId, class: EffectClass, what: &str) -> Vec<Op> {
        let mut ops = Vec::new();
        if self.required(site, class) || self.gated(site) {
            let point = if self.gated(site) {
                Point::OutputGate
            } else {
                Point::BeforeExternal
            };
            ops.push(Op::Flush(point));
        }
        ops.extend(self.run(site, class, what, false));
        ops
    }

    fn reopen(&self, site: SiteId, class: EffectClass, what: &str) -> Vec<Op> {
        let unknown = |seen| Op::Yield {
            site,
            class,
            seen,
            what: what.to_owned(),
        };
        if self.in_step() && class.may_be_unknown() {
            let retry = if self.run_mutation() == RunMutation::VercelRetryRedispatches {
                self.run(site, class, what, true)
            } else {
                vec![
                    Op::Append(Record::Recovery),
                    Op::Flush(Point::Recovery),
                    unknown(Seen::Unknown),
                ]
            };
            return vec![Op::Choice(vec![
                arm(
                    format!("{what} reads attempt 1: the crash came before its dispatch"),
                    Entry::FirstAttempt(site),
                    self.run(site, class, what, true),
                ),
                arm(
                    format!("{what} reads attempt 2 or later"),
                    Entry::LaterAttempt(site),
                    retry,
                ),
            ])];
        }
        if class.may_be_unknown() {
            return match self.run_mutation() {
                RunMutation::RecoveryRedispatchesExternal | RunMutation::RecoverySkipped => {
                    self.rerun(site, class, what)
                }
                RunMutation::RecoveryReportsFailed => vec![unknown(Seen::Failed)],
                _ => vec![unknown(Seen::Unknown)],
            };
        }
        let maybe = matches!(self.handler.sites[site].result, Type::Maybe(_));
        if self.run_mutation() == RunMutation::IdempotentReportedUnknown && maybe {
            return vec![unknown(Seen::Unknown)];
        }
        let mut ops = Vec::new();
        if self.run_mutation() == RunMutation::RerunRewritesCall {
            ops.push(Op::Append(Record::Call(site)));
        }
        ops.extend(self.rerun(site, class, what));
        ops
    }
}
