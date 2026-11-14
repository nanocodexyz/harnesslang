use std::collections::BTreeSet;

use crate::effect::EffectClass;
use crate::ir::{BinaryOp, Expr, Handler, Program, SiteId, SiteKind, Stmt, Type};
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
    pub ops: Vec<Op>,
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
        fenced: model.flush == Flush::FencedTransaction && run != RunMutation::PostgresNoFence,
        checksummed: model.atomic == AtomicUnit::ChecksummedRecord
            && run != RunMutation::JsonlNoChecksum,
        checkpoint_sorted: run != RunMutation::CheckpointHashOrder,
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
        | Expr::Field(_)
        | Expr::Cancelled => false,
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
                | Stmt::Take { .. }
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
            Stmt::Take { .. } => {}
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
