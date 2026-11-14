use std::collections::BTreeSet;

use crate::effect::EffectClass;
use crate::ir::{Program, SiteId};
use crate::prove::lower::{Entry, HandlerPlan, Op, Plan, Record, Seen};
use crate::prove::{Finding, Report};
use crate::target::{AtomicUnit, Others, Recovery};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Running,
    StatusBuffered,
    StatusDurable,
    Acknowledged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Mode {
    Live,
    Replay,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct State {
    mode: Mode,
    phase: Phase,
    pending: BTreeSet<Record>,
    open_calls: BTreeSet<SiteId>,
    durable_calls: BTreeSet<SiteId>,
    reopened: BTreeSet<SiteId>,
    attempt: Option<(SiteId, bool)>,
}

struct Prover<'a> {
    plan: &'a Plan,
    handler: &'a HandlerPlan,
    path: Vec<String>,
    findings: Vec<Finding>,
    external_sites: BTreeSet<SiteId>,
}

pub fn prove(program: &Program, plan: &Plan, report: &mut Report) {
    assert_eq!(
        program.handlers.len(),
        plan.handlers.len(),
        "a plan lowers every handler"
    );
    for handler in &plan.handlers {
        let mut prover = Prover {
            plan,
            handler,
            path: Vec::new(),
            findings: Vec::new(),
            external_sites: BTreeSet::new(),
        };
        prover.conformance();
        let start = State {
            mode: Mode::Live,
            phase: Phase::Running,
            pending: BTreeSet::new(),
            open_calls: BTreeSet::new(),
            durable_calls: BTreeSet::new(),
            reopened: BTreeSet::new(),
            attempt: None,
        };
        let fell = prover.ops(&handler.ops, BTreeSet::from([start]));
        if !fell.is_empty() {
            prover.path.push("the end of the handler".to_owned());
            prover.finding(
                "finish_once",
                "a path ends without a status, a flush and an acknowledgement".to_owned(),
                None,
            );
        }
        let clean = prover.findings.is_empty();
        for finding in prover.findings {
            report.add(finding);
        }
        if clean {
            report.proved.push(format!(
                "handler {} on {}: durability: {} external effect site(s) run only after a flush of their call and every record before it; {} acknowledgement point(s) follow their status flush",
                handler.name,
                plan.model.target.name(),
                prover.external_sites.len(),
                acknowledgements(&handler.ops)
            ));
        }
    }
}

fn acknowledgements(ops: &[Op]) -> usize {
    ops.iter()
        .map(|op| match op {
            Op::Acknowledge => 1,
            Op::Choice(arms) => arms.iter().map(|arm| acknowledgements(&arm.ops)).sum(),
            Op::Repeat { body, .. } => acknowledgements(body),
            _ => 0,
        })
        .sum()
}

fn describe(records: &BTreeSet<Record>, handler: &HandlerPlan) -> String {
    let named: Vec<String> = records
        .iter()
        .map(|record| match record {
            Record::Admitted => "admitted".to_owned(),
            Record::Recovery => "recovery".to_owned(),
            Record::Call(site) => format!("call of {}", handler.sites[*site]),
            Record::Result(site) => format!("result of {}", handler.sites[*site]),
            Record::Model(site) => format!("reply of {}", handler.sites[*site]),
            Record::Status => "status".to_owned(),
            Record::Checkpoint => "checkpoint".to_owned(),
        })
        .collect();
    named.join(", ")
}

impl Prover<'_> {
    fn finding(&mut self, property: &'static str, message: String, counterexample: Option<String>) {
        let path = if self.path.is_empty() {
            "entry".to_owned()
        } else {
            self.path.join(" > ")
        };
        let finding = Finding {
            handler: self.handler.name.clone(),
            target: Some(self.plan.model.target),
            property,
            message,
            path,
            counterexample,
        };
        if !self.findings.contains(&finding) {
            self.findings.push(finding);
        }
    }

    fn conformance(&mut self) {
        let model = self.plan.model;
        if model.others == Others::SecondOwnerAfterLease && !self.plan.fenced {
            self.finding(
                "external_once",
                "flushes are not fenced by the owner's epoch, yet a second owner may run after a lease expires".to_owned(),
                Some("the old owner flushes a call and dispatches it after the new owner recovered the turn".to_owned()),
            );
        }
        if model.atomic == AtomicUnit::ChecksummedRecord && !self.plan.checksummed {
            self.finding(
                "liveness",
                "journal lines carry no checksum, yet a crash may tear the last line".to_owned(),
                Some("a torn line decodes as a different record".to_owned()),
            );
        }
        if !self.plan.checkpoint_sorted {
            self.finding(
                "replay_equal",
                "the checkpoint encodes state in hash order, so two runs write different bytes"
                    .to_owned(),
                None,
            );
        }
    }

    fn ops(&mut self, ops: &[Op], states: BTreeSet<State>) -> BTreeSet<State> {
        let mut current = states;
        for op in ops {
            if current.is_empty() {
                break;
            }
            current = self.op(op, current);
        }
        current
    }

    fn op(&mut self, op: &Op, states: BTreeSet<State>) -> BTreeSet<State> {
        match op {
            Op::Choice(arms) => {
                let mut out = BTreeSet::new();
                for arm in arms {
                    let entered: BTreeSet<State> = states
                        .iter()
                        .filter_map(|state| enter(state.clone(), arm.entry))
                        .collect();
                    if entered.is_empty() {
                        continue;
                    }
                    self.path.push(arm.label.clone());
                    out.extend(self.ops(&arm.ops, entered));
                    self.path.pop();
                }
                out
            }
            Op::Repeat { label, bound, body } => {
                self.path.push(label.clone());
                let mut all = states.clone();
                let mut frontier = states;
                for _ in 0..*bound {
                    let next = self.ops(body, frontier);
                    let new: BTreeSet<State> = next.difference(&all).cloned().collect();
                    if new.is_empty() {
                        break;
                    }
                    all.extend(new.iter().cloned());
                    frontier = new;
                }
                self.path.pop();
                all
            }
            Op::End => {
                for state in states {
                    if state.phase != Phase::Acknowledged {
                        self.finding(
                            "finish_once",
                            "a path ends without acknowledging its finish".to_owned(),
                            None,
                        );
                    }
                }
                BTreeSet::new()
            }
            _ => states
                .into_iter()
                .map(|state| self.step(op, state))
                .collect(),
        }
    }

    fn step(&mut self, op: &Op, mut state: State) -> State {
        match op {
            Op::Append(record) => self.append(*record, &mut state),
            Op::Flush(_) => {
                state.durable_calls.extend(state.open_calls.iter().copied());
                if state.phase == Phase::StatusBuffered && state.pending.contains(&Record::Status) {
                    state.phase = Phase::StatusDurable;
                }
                state.pending.clear();
            }
            Op::Run {
                site,
                class,
                outbound,
                what,
            } => self.run(*site, *class, *outbound, what, &mut state),
            Op::Yield {
                site,
                class,
                seen,
                what,
            } => {
                self.yielded(*class, *seen, what);
                state.reopened.remove(site);
                state.open_calls.remove(site);
                state.durable_calls.remove(site);
            }
            Op::Acknowledge => self.acknowledge(&mut state),
            Op::Choice(_) | Op::Repeat { .. } | Op::End => {
                unreachable!("structured ops are walked by op")
            }
        }
        state
    }

    fn append(&mut self, record: Record, state: &mut State) {
        match record {
            Record::Call(site) => {
                if !state.open_calls.insert(site) {
                    self.finding(
                        "seq_unique",
                        format!(
                            "{} appends a second call record for a sequence number whose call is already in the journal",
                            self.handler.sites[site]
                        ),
                        None,
                    );
                }
            }
            Record::Result(site) | Record::Model(site) => {
                state.open_calls.remove(&site);
                state.durable_calls.remove(&site);
                state.reopened.remove(&site);
            }
            Record::Status => {
                if state.phase != Phase::Running {
                    self.finding(
                        "finish_once",
                        "a path appends a second status record".to_owned(),
                        None,
                    );
                }
                state.phase = Phase::StatusBuffered;
            }
            Record::Admitted | Record::Recovery | Record::Checkpoint => {}
        }
        state.pending.insert(record);
    }

    fn run(
        &mut self,
        site: SiteId,
        class: EffectClass,
        outbound: bool,
        what: &str,
        state: &mut State,
    ) {
        let pending = describe(&state.pending, self.handler);
        if outbound && self.plan.model.gate_before_outbound && !state.pending.is_empty() {
            self.finding(
                "output_gate",
                format!("{what} leaves the object before the output gate commits: {pending}"),
                None,
            );
        }
        if class == EffectClass::External {
            self.external(site, what, state, &pending);
        }
        state.attempt = None;
    }

    fn external(&mut self, site: SiteId, what: &str, state: &State, pending: &str) {
        self.external_sites.insert(site);
        if !state.pending.is_empty() {
            self.finding(
                "durable_before_external",
                format!("external {what} runs while records are not durable: {pending}"),
                Some(format!(
                    "a crash right after the dispatch loses {pending}, so recovery cannot report the call unknown and may run it again"
                )),
            );
        } else if !state.durable_calls.contains(&site) {
            self.finding(
                "durable_before_external",
                format!("external {what} runs before its call record is durable"),
                None,
            );
        }
        let recovery = self.plan.model.recovery;
        if state.reopened.contains(&site) && recovery == Recovery::AtLoad {
            self.finding(
                "external_once",
                format!("external {what} runs again after recovery found its call open"),
                Some("the first run may have dispatched it before the crash".to_owned()),
            );
        }
        if let Recovery::InStep { .. } = recovery {
            match state.attempt {
                Some((attempted, true)) if attempted == site => {}
                Some((attempted, false)) if attempted == site => self.finding(
                    "external_once",
                    format!("external {what} runs again on a retried step"),
                    Some("attempt 1 may have dispatched it before the step was retried".to_owned()),
                ),
                _ => self.finding(
                    "external_once",
                    format!("external {what} runs without reading its step attempt first"),
                    None,
                ),
            }
        }
    }

    fn yielded(&mut self, class: EffectClass, seen: Seen, what: &str) {
        match (class.may_be_unknown(), seen) {
            (true, Seen::Unknown) | (false, Seen::Failed) => {}
            (true, Seen::Failed) => self.finding(
                "unknown_reported",
                format!("external {what}, reopened after a crash, is reported failed although it may have run"),
                None,
            ),
            (false, Seen::Unknown) => self.finding(
                "idempotent_known",
                format!("{} {what}, reopened after a crash, is reported unknown although it is safe to run again", class.name()),
                None,
            ),
        }
    }

    fn acknowledge(&mut self, state: &mut State) {
        match state.phase {
            Phase::StatusDurable => state.phase = Phase::Acknowledged,
            Phase::Acknowledged => {
                self.finding("finish_once", "a path acknowledges twice".to_owned(), None);
            }
            Phase::Running | Phase::StatusBuffered => {
                self.finding(
                    "finish_once",
                    "the finish is acknowledged before its status record is durable".to_owned(),
                    Some("a crash after the acknowledgement loses the status, so the turn runs again after its caller saw it finish".to_owned()),
                );
                state.phase = Phase::Acknowledged;
            }
        }
    }
}

fn enter(mut state: State, entry: Entry) -> Option<State> {
    match entry {
        Entry::Plain => {}
        Entry::Live => state.mode = Mode::Live,
        Entry::Rerun => state.mode = Mode::Replay,
        Entry::Replayed => {
            if state.mode != Mode::Replay {
                return None;
            }
        }
        Entry::Reopened(site) => {
            if state.mode != Mode::Replay {
                return None;
            }
            state.mode = Mode::Live;
            state.reopened.insert(site);
            state.open_calls.insert(site);
            state.durable_calls.insert(site);
        }
        Entry::FirstAttempt(site) => state.attempt = Some((site, true)),
        Entry::LaterAttempt(site) => state.attempt = Some((site, false)),
    }
    Some(state)
}
