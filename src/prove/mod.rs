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
            | Stmt::KeepBlobs { .. }
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
        children(&program, &plan, mutation, &mut report);
    }
    report
}

fn spawning(program: &Program, kind: crate::ir::SiteKind) -> bool {
    match kind {
        crate::ir::SiteKind::Tool(tool) => program.tools[tool].spawn.is_some(),
        crate::ir::SiteKind::Dispatch => program.spawns().next().is_some(),
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PlannedTree {
    records: u64,
    requests: u64,
    sessions: u64,
}

fn spawns_on(program: &Program, handler: &crate::ir::Handler, ops: &[Op]) -> u64 {
    let spawns = |op: &Op| match op {
        Op::Run {
            slot: lower::Slot::Site(site),
            ..
        } => Count {
            total: u64::from(spawning(program, handler.sites[*site].kind)),
            nested: 0,
        },
        _ => Count::default(),
    };
    planned_max(ops, &spawns)
        .expect("the IR's child session count fits u64, and the plan starts no more")
        .total
}

struct Descent<'a> {
    model: &'a target::Model,
    mutation: Mutation,
    root: &'a str,
}

fn planned_tree(
    descent: &Descent<'_>,
    program: &Program,
    handler: &crate::ir::Handler,
    ops: &[Op],
    chain: &str,
    report: &mut Report,
) -> Option<PlannedTree> {
    let model = descent.model;
    let records = planned_max(ops, &|op| appended(program, op))
        .expect("the IR's record count fits u64, and the plan appends no more")
        .total;
    let requests = planned_max(ops, &outbound)?.total;
    let mut tree = PlannedTree {
        records,
        requests,
        sessions: 1,
    };
    let spawns = spawns_on(program, handler, ops);
    if spawns == 0 {
        return Some(tree);
    }
    let mut widest = PlannedTree::default();
    for (tool, spawn) in program.spawns() {
        let child_program = &spawn.program;
        let child_plan = lower::lower(child_program, model, descent.mutation);
        let index = child_program
            .handlers
            .iter()
            .position(|declared| declared.name == spawn.handler)
            .expect("the checker spawns only a handler the child harness declares");
        let child = &child_program.handlers[index];
        let child_chain = format!(
            "{chain} > {} runs {}.{}",
            tool.name,
            spawn.harness(),
            spawn.handler
        );
        let child_tree = planned_tree(
            descent,
            child_program,
            child,
            &child_plan.handlers[index].ops,
            &child_chain,
            report,
        )?;
        if let Some(limit) = model.limits.outbound_requests_per_invocation {
            let own = planned_max(&child_plan.handlers[index].ops, &outbound)?.total;
            if own > limit.value {
                report.add(Finding {
                    handler: descent.root.to_owned(),
                    target: Some(model.target),
                    property: "worst_case",
                    message: format!(
                        "child session {}.{} that {} starts has worst case {} outbound requests, over {}'s limit of {} {}",
                        spawn.harness(),
                        spawn.handler,
                        tool.name,
                        own,
                        model.target.name(),
                        limit.value,
                        limit.unit
                    ),
                    path: child.worst.requests.describe_loops(),
                    counterexample: None,
                });
                return None;
            }
        }
        if let Some(limit) = &spawn.sessions
            && child_tree.sessions > limit.value
        {
            report.add(Finding {
                handler: descent.root.to_owned(),
                target: Some(model.target),
                property: "worst_case",
                message: format!(
                    "one call of {} starts {} sessions on the plans' longest paths, over sessions at most {} ({})",
                    tool.name, child_tree.sessions, limit.name, limit.value
                ),
                path: child_chain,
                counterexample: Some(format!(
                    "{}.{} and {} descendant session(s), each running its handler's longest path",
                    spawn.harness(),
                    spawn.handler,
                    child_tree.sessions - 1
                )),
            });
            return None;
        }
        widest.records = widest.records.max(child_tree.records);
        widest.requests = widest.requests.max(child_tree.requests);
        widest.sessions = widest.sessions.max(child_tree.sessions);
    }
    tree.records = tree
        .records
        .checked_add(spawns.checked_mul(widest.records)?)?;
    tree.requests = tree
        .requests
        .checked_add(spawns.checked_mul(widest.requests)?)?;
    tree.sessions = tree
        .sessions
        .checked_add(spawns.checked_mul(widest.sessions)?)?;
    Some(tree)
}

fn children(program: &Program, plan: &Plan, mutation: Mutation, report: &mut Report) {
    let model = &plan.model;
    for (handler, lowered) in program.handlers.iter().zip(&plan.handlers) {
        let Some(family) = &handler.worst.family else {
            continue;
        };
        let planned = spawns_on(program, handler, &lowered.ops);
        assert!(
            planned <= handler.worst.spawns.total,
            "the IR's child session worst case covers every path of the plan"
        );
        let descent = Descent {
            model,
            mutation,
            root: &handler.name,
        };
        let chain = format!("{}.{}", program.name, handler.name);
        let Some(tree) = planned_tree(&descent, program, handler, &lowered.ops, &chain, report)
        else {
            continue;
        };
        assert!(
            tree.records <= family.total[1],
            "the IR's family record worst case covers every path of the plans"
        );
        assert!(
            tree.requests <= family.total[2],
            "the IR's family request worst case covers every path of the plans"
        );
        assert!(
            tree.sessions - 1 <= family.sessions,
            "the IR's family session worst case covers every path of the plans"
        );
        report.proved.push(format!(
            "handler {} on {}: worst_case: {planned} child session(s) on the plan's longest path, as hl check counts ({}), each in its own run; with them {} effects, {} journal records, {} outbound requests",
            handler.name,
            model.target.name(),
            handler.worst.spawns.describe_loops(),
            family.total[0],
            family.total[1],
            family.total[2]
        ));
        if family.depth > 1 {
            report.proved.push(format!(
                "handler {} on {}: worst_case: {} sessions in its tree on the plans' longest paths, {} journal records and {} outbound requests with them, as hl check counts; every spawn within its sessions at most",
                handler.name,
                model.target.name(),
                tree.sessions - 1,
                tree.records,
                tree.requests
            ));
        }
    }
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
