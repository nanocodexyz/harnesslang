pub mod arith;
pub mod cells;
pub mod cli;
pub mod durability;
pub mod finish;
pub mod linear;
pub mod lower;
pub mod text;

use crate::effect::EffectClass;
use crate::ir::worst::{ENTRY_UPDATE_RECORDS, NESTED_LOG_RECORDS, TIMER_UPDATE_RECORDS};
use crate::ir::{Program, Stmt};
use crate::prove::lower::{Op, Plan, Record, Slot};
use crate::run;
use crate::target::{self, Target};

pub const PROOF_STEPS_PER_HANDLER_MAX: u64 = 4_000_000;
pub const PROOF_CONSTRAINTS_MAX: usize = 4_096;
pub const PROOF_MEMO_ENTRIES_PER_HANDLER_MAX: usize = 16_384;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub handler: String,
    pub target: Option<Target>,
    pub property: &'static str,
    pub message: String,
    pub path: String,
    pub counterexample: Option<String>,
}

impl Finding {
    pub fn render(&self) -> String {
        let mut text = format!("handler '{}'", self.handler);
        if let Some(target) = self.target {
            text.push_str(&format!(" on {}", target.name()));
        }
        text.push_str(&format!(
            ": {}: {}; path: {}",
            self.property, self.message, self.path
        ));
        if let Some(counterexample) = &self.counterexample {
            text.push_str(&format!("; counterexample: {counterexample}"));
        }
        text
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub proved: Vec<String>,
}

impl Report {
    pub fn add(&mut self, finding: Finding) {
        if !self.findings.contains(&finding) {
            self.findings.push(finding);
        }
    }

    pub fn errors(&self) -> usize {
        self.findings.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mutation {
    None,
    Lowering(run::mutation::Mutation),
    DropFlush,
    DropFinish,
    DropRequires,
    RepeatAssign,
    DropCellFinish,
}

const LOWERING_MUTATIONS: [run::mutation::Mutation; 28] = [
    run::mutation::Mutation::FlushAfterDispatch,
    run::mutation::Mutation::RecoveryRedispatchesExternal,
    run::mutation::Mutation::VercelRetryRedispatches,
    run::mutation::Mutation::PostgresNoFence,
    run::mutation::Mutation::JsonlNoChecksum,
    run::mutation::Mutation::FinishBeforeFlush,
    run::mutation::Mutation::IdempotentReportedUnknown,
    run::mutation::Mutation::GateReleasesEarly,
    run::mutation::Mutation::RecoveryReportsFailed,
    run::mutation::Mutation::RerunRewritesCall,
    run::mutation::Mutation::CheckpointHashOrder,
    run::mutation::Mutation::RecoverySkipped,
    run::mutation::Mutation::CellHeaderRidesPastExternal,
    run::mutation::Mutation::NestedFlushAfterDispatch,
    run::mutation::Mutation::CellRecoveryRedispatchesExternal,
    run::mutation::Mutation::CellRecoveryReportsFailed,
    run::mutation::Mutation::CellReplayIgnoresJournal,
    run::mutation::Mutation::CellRerunAfterFinish,
    run::mutation::Mutation::TimeoutRides,
    run::mutation::Mutation::DivergenceDispatches,
    run::mutation::Mutation::NestedIdsRestart,
    run::mutation::Mutation::CellLimitUnchecked,
    run::mutation::Mutation::VercelNestedRetryRedispatches,
    run::mutation::Mutation::CellGateReleasesEarly,
    run::mutation::Mutation::PostgresCellUnfenced,
    run::mutation::Mutation::NestedIdempotentReportedUnknown,
    run::mutation::Mutation::SuspendBeforeFlush,
    run::mutation::Mutation::ResumeReplaysSuspend,
];

const PROGRAM_MUTATIONS: [Mutation; 5] = [
    Mutation::DropFlush,
    Mutation::DropFinish,
    Mutation::DropRequires,
    Mutation::RepeatAssign,
    Mutation::DropCellFinish,
];

impl Mutation {
    pub fn all() -> Vec<Self> {
        let mut all = vec![Self::None];
        all.extend(LOWERING_MUTATIONS.map(Self::Lowering));
        all.extend(PROGRAM_MUTATIONS);
        all
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Lowering(mutation) => mutation.name(),
            Self::DropFlush => "drop_flush",
            Self::DropFinish => "drop_finish",
            Self::DropRequires => "drop_requires",
            Self::RepeatAssign => "repeat_assign",
            Self::DropCellFinish => "drop_cell_finish",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub fn names() -> String {
        Self::all()
            .into_iter()
            .map(Self::name)
            .collect::<Vec<_>>()
            .join("|")
    }

    pub fn targets(self) -> Vec<Target> {
        match self {
            Self::Lowering(mutation) => mutation.targets(),
            _ => Target::ALL.to_vec(),
        }
    }

    pub const fn lowering(self) -> run::mutation::Mutation {
        match self {
            Self::Lowering(mutation) => mutation,
            _ => run::mutation::Mutation::None,
        }
    }
}

fn repeat_assignments(statements: &mut Vec<Stmt>) {
    let mut out = Vec::with_capacity(statements.len());
    for mut statement in statements.drain(..) {
        match &mut statement {
            Stmt::Assign { .. } => out.push(statement.clone()),
            Stmt::If {
                then, otherwise, ..
            } => {
                repeat_assignments(then);
                repeat_assignments(otherwise);
            }
            Stmt::Match { arms, .. } => {
                arms.iter_mut()
                    .for_each(|arm| repeat_assignments(&mut arm.body));
            }
            Stmt::Loop { body, .. } | Stmt::For { body, .. } => repeat_assignments(body),
            Stmt::Parallel(_)
            | Stmt::Let { .. }
            | Stmt::Push { .. }
            | Stmt::Shrink { .. }
            | Stmt::Effect { .. }
            | Stmt::Take { .. }
            | Stmt::Finish { .. } => {}
        }
        out.push(statement);
    }
    *statements = out;
}

pub fn mutate(program: &Program, mutation: Mutation) -> Program {
    let mut program = program.clone();
    for handler in &mut program.handlers {
        match mutation {
            Mutation::DropFinish => {
                if matches!(handler.body.last(), Some(Stmt::Finish { .. })) {
                    handler.body.pop();
                }
            }
            Mutation::DropRequires => handler.requires = None,
            Mutation::RepeatAssign => repeat_assignments(&mut handler.body),
            Mutation::None
            | Mutation::Lowering(_)
            | Mutation::DropFlush
            | Mutation::DropCellFinish => {}
        }
    }
    program
}

pub fn prove(program: &Program, targets: &[Target], mutation: Mutation) -> Report {
    let program = mutate(program, mutation);
    let mut report = Report::default();
    finish::prove(&program, &mut report);
    arith::prove(&program, &mut report);
    cells::prove(&program, &mut report);
    for target in targets {
        let model = target::model(*target);
        let plan = lower::lower(&program, &model, mutation);
        durability::prove(&program, &plan, &mut report);
        records(&program, &plan, mutation, &mut report);
        bounds(&program, &plan, &mut report);
        waits(&program, &plan, &mut report);
    }
    report
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Count {
    total: u64,
    nested: u64,
}

impl Count {
    fn plus(self, other: Self) -> Option<Self> {
        Some(Self {
            total: self.total.checked_add(other.total)?,
            nested: self.nested.checked_add(other.nested)?,
        })
    }

    fn times(self, bound: u64) -> Option<Self> {
        Some(Self {
            total: self.total.checked_mul(bound)?,
            nested: self.nested.checked_mul(bound)?,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct Planned {
    through: Option<Count>,
    ended: Option<Count>,
}

fn larger(left: Option<Count>, right: Option<Count>) -> Option<Count> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, None) => left,
        (None, right) => right,
    }
}

fn after(before: Count, cost: Option<Count>) -> Option<Option<Count>> {
    match cost {
        Some(cost) => Some(Some(before.plus(cost)?)),
        None => Some(None),
    }
}

fn then(first: Planned, second: Planned) -> Option<Planned> {
    let Some(before) = first.through else {
        return Some(first);
    };
    Some(Planned {
        through: after(before, second.through)?,
        ended: larger(first.ended, after(before, second.ended)?),
    })
}

fn repeated(body: Planned, bound: u64, every_iteration: bool) -> Option<Planned> {
    if bound == 0 {
        return Some(Planned {
            through: Some(Count::default()),
            ended: None,
        });
    }
    let through = match body.through {
        Some(each) => Some(each.times(bound)?),
        None if every_iteration => None,
        None => Some(Count::default()),
    };
    let before = body.through.unwrap_or_default().times(bound - 1)?;
    Some(Planned {
        through,
        ended: after(before, body.ended)?,
    })
}

fn planned(ops: &[Op], cost: &dyn Fn(&Op) -> Count) -> Option<Planned> {
    let mut paths = Planned {
        through: Some(Count::default()),
        ended: None,
    };
    for op in ops {
        let step = match op {
            Op::Choice(arms) => {
                let mut widest = Planned {
                    through: None,
                    ended: None,
                };
                for arm in arms {
                    let one = planned(&arm.ops, cost)?;
                    widest = Planned {
                        through: larger(widest.through, one.through),
                        ended: larger(widest.ended, one.ended),
                    };
                }
                widest
            }
            Op::Repeat {
                bound,
                every_iteration,
                body,
                ..
            } => repeated(planned(body, cost)?, *bound, *every_iteration)?,
            Op::End | Op::Park { .. } => Planned {
                through: None,
                ended: Some(Count::default()),
            },
            Op::Append(_)
            | Op::Flush(_)
            | Op::FlushExcept(..)
            | Op::Run { .. }
            | Op::Yield { .. }
            | Op::Replay(_)
            | Op::Wait { .. }
            | Op::Sleep { .. }
            | Op::CloseCell(..)
            | Op::Acknowledge => Planned {
                through: Some(cost(op)),
                ended: None,
            },
        };
        paths = then(paths, step)?;
    }
    Some(paths)
}

fn planned_max(ops: &[Op], cost: &dyn Fn(&Op) -> Count) -> Option<Count> {
    let paths = planned(ops, cost)?;
    larger(paths.through, paths.ended)
}

fn outbound(op: &Op) -> Count {
    match op {
        Op::Run { slot, outbound, .. } => Count {
            total: u64::from(*outbound),
            nested: u64::from(*outbound && slot.nested()),
        },
        _ => Count::default(),
    }
}

fn slept(op: &Op) -> Count {
    match op {
        Op::Sleep { ms, .. } => Count {
            total: *ms,
            nested: 0,
        },
        _ => Count::default(),
    }
}

fn waits(program: &Program, plan: &Plan, report: &mut Report) {
    let model = &plan.model;
    let Some(crate::target::WaitLimit { limit, scope }) = model.limits.wait_ms else {
        return;
    };
    for (handler, lowered) in program.handlers.iter().zip(&plan.handlers) {
        let counted = &handler.worst.wait_ms;
        if counted.total == 0 {
            continue;
        }
        let Some(planned) = planned_max(&lowered.ops, &slept) else {
            report.add(Finding {
                handler: handler.name.clone(),
                target: Some(model.target),
                property: "worst_case",
                message: "the worst case of milliseconds of waits overflows u64".to_owned(),
                path: counted.describe_loops(),
                counterexample: None,
            });
            continue;
        };
        assert!(
            planned.total <= counted.total,
            "the IR's wait worst case covers every path of the plan"
        );
        let (measured, what) = match scope {
            crate::target::WaitScope::Step => (
                crate::target::longest_wait(program, handler).map_or(0, |(_, value)| value),
                "milliseconds in its longest wait",
            ),
        };
        if measured > limit.value {
            report.add(Finding {
                handler: handler.name.clone(),
                target: Some(model.target),
                property: "worst_case",
                message: format!(
                    "worst case {measured} {what} exceeds {}'s limit of {} {}",
                    model.target.name(),
                    limit.value,
                    limit.unit
                ),
                path: counted.describe_loops(),
                counterexample: None,
            });
            continue;
        }
        report.proved.push(format!(
            "handler {} on {}: worst_case: {measured} {what} <= {} {} ({})",
            handler.name,
            model.target.name(),
            limit.value,
            limit.unit,
            counted.describe_loops()
        ));
    }
}

fn appended(program: &Program, op: &Op) -> Count {
    let total = match op {
        Op::Append(Record::Call(Slot::Nested(_, tool)) | Record::Result(Slot::Nested(_, tool))) => {
            let logged = program.tools[*tool].effect == EffectClass::External;
            ENTRY_UPDATE_RECORDS + if logged { NESTED_LOG_RECORDS } else { 0 }
        }
        Op::Append(Record::Timeout(_)) => 0,
        Op::Append(_) => 1,
        Op::CloseCell(_, code) => program.codes[*code]
            .timers
            .value
            .checked_mul(TIMER_UPDATE_RECORDS)
            .expect("hl check counted the cell's timer records in u64"),
        _ => 0,
    };
    Count { total, nested: 0 }
}

fn records(program: &Program, plan: &Plan, mutation: Mutation, report: &mut Report) {
    let model = &plan.model;
    for (handler, lowered) in program.handlers.iter().zip(&plan.handlers) {
        let counted = &handler.worst.records;
        let planned = planned_max(&lowered.ops, &|op| appended(program, op))
            .expect("the IR's record count fits u64, and the plan appends no more")
            .total;
        if mutation == Mutation::Lowering(run::mutation::Mutation::DivergenceDispatches) {
            continue;
        }
        assert!(
            planned <= counted.total,
            "the IR's journal record worst case covers every path of the plan"
        );
        if mutation == Mutation::None {
            assert_eq!(
                planned, counted.total,
                "the IR's journal record worst case is a path of the plan"
            );
        }
        report.proved.push(format!(
            "handler {} on {}: worst_case: {} journal records per run on the plan's longest path, as hl check counts ({})",
            handler.name,
            model.target.name(),
            planned,
            counted.describe_loops()
        ));
    }
}

fn bounds(program: &Program, plan: &Plan, report: &mut Report) {
    let model = &plan.model;
    let Some(limit) = model.limits.outbound_requests_per_invocation else {
        return;
    };
    for (handler, lowered) in program.handlers.iter().zip(&plan.handlers) {
        let requests = &handler.worst.requests;
        let Some(planned) = planned_max(&lowered.ops, &outbound) else {
            report.add(Finding {
                handler: handler.name.clone(),
                target: Some(model.target),
                property: "worst_case",
                message: "the worst case of outbound requests overflows u64".to_owned(),
                path: requests.describe_loops(),
                counterexample: None,
            });
            continue;
        };
        let nested = if planned.nested == 0 {
            String::new()
        } else {
            format!(" ({} of them nested calls in cells)", planned.nested)
        };
        let planned = planned.total;
        assert!(
            planned <= requests.total,
            "the IR's worst case covers every path of the plan"
        );
        if planned > limit.value {
            report.add(Finding {
                handler: handler.name.clone(),
                target: Some(model.target),
                property: "worst_case",
                message: format!(
                    "worst case {} outbound requests{nested} exceeds {}'s limit of {} {}",
                    planned,
                    model.target.name(),
                    limit.value,
                    limit.unit
                ),
                path: requests.describe_loops(),
                counterexample: None,
            });
            continue;
        }
        report.proved.push(format!(
            "handler {} on {}: worst_case: {} outbound requests{nested} <= {} {} ({})",
            handler.name,
            model.target.name(),
            planned,
            limit.value,
            limit.unit,
            requests.describe_loops()
        ));
    }
}
