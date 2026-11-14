pub mod arith;
pub mod cli;
pub mod durability;
pub mod finish;
pub mod linear;
pub mod lower;
pub mod text;

use crate::ir::{Program, Stmt};
use crate::prove::lower::{Op, Plan};
use crate::run;
use crate::target::{self, Target};

pub const PROOF_STEPS_PER_HANDLER_MAX: u64 = 4_000_000;
pub const PROOF_CONSTRAINTS_MAX: usize = 4_096;

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
}

const LOWERING_MUTATIONS: [run::mutation::Mutation; 12] = [
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
];

const PROGRAM_MUTATIONS: [Mutation; 4] = [
    Mutation::DropFlush,
    Mutation::DropFinish,
    Mutation::DropRequires,
    Mutation::RepeatAssign,
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
            Stmt::Let { .. } | Stmt::Push { .. } | Stmt::Effect { .. } | Stmt::Finish { .. } => {}
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
            Mutation::None | Mutation::Lowering(_) | Mutation::DropFlush => {}
        }
    }
    program
}

pub fn prove(program: &Program, targets: &[Target], mutation: Mutation) -> Report {
    let program = mutate(program, mutation);
    let mut report = Report::default();
    finish::prove(&program, &mut report);
    arith::prove(&program, &mut report);
    for target in targets {
        let model = target::model(*target);
        let plan = lower::lower(&program, &model, mutation);
        durability::prove(&program, &plan, &mut report);
        bounds(&program, &plan, &mut report);
    }
    report
}

fn outbound_max(ops: &[Op]) -> Option<u64> {
    let mut total: u64 = 0;
    for op in ops {
        let cost = match op {
            Op::Run { outbound, .. } => u64::from(*outbound),
            Op::Choice(arms) => {
                let mut widest = 0;
                for arm in arms {
                    widest = widest.max(outbound_max(&arm.ops)?);
                }
                widest
            }
            Op::Repeat { bound, body, .. } => outbound_max(body)?.checked_mul(*bound)?,
            Op::Append(_) | Op::Flush(_) | Op::Yield { .. } | Op::Acknowledge | Op::End => 0,
        };
        total = total.checked_add(cost)?;
    }
    Some(total)
}

fn bounds(program: &Program, plan: &Plan, report: &mut Report) {
    let model = &plan.model;
    let Some(limit) = model.limits.outbound_requests_per_invocation else {
        return;
    };
    for (handler, lowered) in program.handlers.iter().zip(&plan.handlers) {
        let requests = &handler.worst.requests;
        let Some(planned) = outbound_max(&lowered.ops) else {
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
                    "worst case {} outbound requests exceeds {}'s limit of {} {}",
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
            "handler {} on {}: worst_case: {} outbound requests <= {} {} ({})",
            handler.name,
            model.target.name(),
            planned,
            limit.value,
            limit.unit,
            requests.describe_loops()
        ));
    }
}
