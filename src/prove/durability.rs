use std::collections::{BTreeMap, BTreeSet};

use crate::effect::EffectClass;
use crate::ir::{Program, SiteId};
use crate::prove::lower::{Entry, HandlerPlan, Op, Plan, Record, Seen, Slot};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CellPhase {
    Recorded,
    Running,
    Diverged,
    Finished,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct State {
    mode: Mode,
    phase: Phase,
    recovered: bool,
    pending: BTreeSet<Record>,
    open_calls: BTreeSet<Slot>,
    durable_calls: BTreeSet<Slot>,
    reopened: BTreeSet<Slot>,
    recorded: BTreeSet<Slot>,
    cells: BTreeMap<SiteId, CellPhase>,
    attempt: Option<(Slot, bool)>,
}

impl State {
    fn start() -> Self {
        Self {
            mode: Mode::Live,
            phase: Phase::Running,
            recovered: false,
            pending: BTreeSet::new(),
            open_calls: BTreeSet::new(),
            durable_calls: BTreeSet::new(),
            reopened: BTreeSet::new(),
            recorded: BTreeSet::new(),
            cells: BTreeMap::new(),
            attempt: None,
        }
    }

    fn settle(&mut self, slot: Slot) {
        self.open_calls.remove(&slot);
        self.durable_calls.remove(&slot);
        self.reopened.remove(&slot);
    }
}

struct Prover<'a> {
    plan: &'a Plan,
    handler: &'a HandlerPlan,
    path: Vec<String>,
    findings: Vec<Finding>,
    external_sites: BTreeSet<Slot>,
    waits: BTreeSet<SiteId>,
    cells: BTreeSet<SiteId>,
    sleeps: BTreeSet<Slot>,
    parks: BTreeSet<Slot>,
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
            waits: BTreeSet::new(),
            cells: BTreeSet::new(),
            sleeps: BTreeSet::new(),
            parks: BTreeSet::new(),
        };
        prover.conformance();
        let fell = prover.ops(&handler.ops, BTreeSet::from([State::start()]));
        if !fell.is_empty() {
            prover.path.push("the end of the handler".to_owned());
            prover.finding(
                "finish_once",
                "a path ends without a status, a flush and an acknowledgement".to_owned(),
                None,
            );
        }
        prover.report(report);
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

fn has_cells(ops: &[Op]) -> bool {
    ops.iter().any(|op| match op {
        Op::CloseCell(..) => true,
        Op::Choice(arms) => arms.iter().any(|arm| has_cells(&arm.ops)),
        Op::Repeat { body, .. } => has_cells(body),
        _ => false,
    })
}

impl Prover<'_> {
    fn report(self, report: &mut Report) {
        let clean = self.findings.is_empty();
        let name = &self.handler.name;
        let target = self.plan.model.target.name();
        let nested = self
            .external_sites
            .iter()
            .filter(|slot| slot.nested())
            .count();
        for finding in self.findings {
            report.add(finding);
        }
        if !clean {
            return;
        }
        report.proved.push(format!(
            "handler {name} on {target}: durability: {} external effect site(s) run only after a flush of their call and every record before it; {} acknowledgement point(s) follow their status flush",
            self.external_sites.len() - nested,
            acknowledgements(&self.handler.ops)
        ));
        if !self.sleeps.is_empty() || !self.parks.is_empty() {
            report.proved.push(format!(
                "handler {name} on {target}: parks: {} wait site(s) sleep only after their call and every record before it are durable; {} suspend site(s) end the run only with nothing buffered and are never appended twice",
                self.sleeps.len(),
                self.parks.len()
            ));
        }
        if self.cells.is_empty() {
            return;
        }
        report.proved.push(format!(
            "handler {name} on {target}: cells: {} code site(s); {nested} nested external tool(s) run only after their cell's header, entries and call record are durable and never again once reopened, replayed or past a divergence; {} timer wait site(s) follow a durable header; every path appends one finished update and one site result",
            self.cells.len(),
            self.waits.len()
        ));
    }

    fn slot_name(&self, slot: Slot) -> String {
        match slot {
            Slot::Site(site) => self.handler.sites[site].clone(),
            Slot::Nested(site, tool) => format!(
                "tools.{} in the cell of {}",
                self.plan.tools[tool], self.handler.sites[site]
            ),
        }
    }

    fn describe(&self, records: &BTreeSet<Record>) -> String {
        let named: Vec<String> = records
            .iter()
            .map(|record| match record {
                Record::Admitted => "admitted".to_owned(),
                Record::Recovery => "recovery".to_owned(),
                Record::Call(slot) => format!("call of {}", self.slot_name(*slot)),
                Record::Result(slot) => format!("result of {}", self.slot_name(*slot)),
                Record::Model(site) => format!("reply of {}", self.handler.sites[*site]),
                Record::BatchCall(site) => format!("call of {}", self.handler.sites[*site]),
                Record::BatchResult(site) => format!("result of {}", self.handler.sites[*site]),
                Record::Status => "status".to_owned(),
                Record::Checkpoint => "checkpoint".to_owned(),
                Record::CellHeader(site) => {
                    format!("cell header of {}", self.handler.sites[*site])
                }
                Record::Timeout(site) => format!("timeout of {}", self.handler.sites[*site]),
                Record::CellFinished(site) => {
                    format!("cell finish of {}", self.handler.sites[*site])
                }
            })
            .collect();
        named.join(", ")
    }

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
        if has_cells(&self.handler.ops) {
            self.cell_conformance();
        }
    }

    fn cell_conformance(&mut self) {
        let model = self.plan.model;
        if model.others == Others::SecondOwnerAfterLease && !self.plan.cells_fenced {
            self.finding(
                "external_once",
                "a cell's flushes are not fenced by the owner's epoch, yet a second owner may run after a lease expires".to_owned(),
                Some("the old owner's engine flushes a nested call and dispatches it after the new owner reported it unknown".to_owned()),
            );
        }
        if !self.plan.nested_ids_continue {
            self.finding(
                "seq_unique",
                "a resumed cell numbers its nested calls from 1, so a live call after a replayed one reuses a journaled id".to_owned(),
                None,
            );
        }
        if !self.plan.cell_limits_checked {
            self.finding(
                "cell_bounds",
                "the runner does not enforce a code tool's calls, timers and journal_bytes, so a cell's nested calls, waits and journal are unbounded".to_owned(),
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
            Op::Repeat {
                label, bound, body, ..
            } => {
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
            Op::Park { slot, what } => {
                for state in states {
                    self.park(*slot, what, &state);
                }
                BTreeSet::new()
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
            Op::Flush(_) => flush(&mut state, None),
            Op::FlushExcept(_, kept) => flush(&mut state, Some(*kept)),
            Op::Run {
                slot,
                class,
                outbound,
                what,
            } => self.run(*slot, *class, *outbound, what, &mut state),
            Op::Yield {
                slot,
                class,
                seen,
                what,
            } => {
                self.yielded(*slot, *class, *seen, what, &state);
                state.settle(*slot);
            }
            Op::Replay(slot) => {
                assert!(
                    state.recorded.remove(slot),
                    "a replayed nested call entered through its journaled result"
                );
            }
            Op::Wait { site, what } => self.wait(*site, what, &state),
            Op::Sleep { slot, what, .. } => self.sleep(*slot, what, &state),
            Op::CloseCell(site, _) => self.close_cell(*site, &mut state),
            Op::Acknowledge => self.acknowledge(&mut state),
            Op::Choice(_) | Op::Repeat { .. } | Op::End | Op::Park { .. } => {
                unreachable!("structured ops are walked by op")
            }
        }
        state
    }

    fn append(&mut self, record: Record, state: &mut State) {
        match record {
            Record::Call(slot) => {
                if !state.open_calls.insert(slot) || state.recorded.contains(&slot) {
                    self.finding(
                        "seq_unique",
                        format!(
                            "{} appends a second call record for an id whose call is already in the journal",
                            self.slot_name(slot)
                        ),
                        None,
                    );
                }
            }
            Record::Result(slot) => {
                state.settle(slot);
                if let Slot::Site(site) = slot {
                    self.site_result(site, state);
                }
            }
            Record::Model(site) => state.settle(Slot::Site(site)),
            Record::BatchCall(site) => {
                state.open_calls.insert(Slot::Site(site));
            }
            Record::BatchResult(_) => {}
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
            Record::Recovery => state.recovered = true,
            Record::CellHeader(site) => self.header(site, state),
            Record::CellFinished(site) => self.cell_finished(site, state),
            Record::Admitted | Record::Checkpoint | Record::Timeout(_) => {}
        }
        state.pending.insert(record);
    }

    fn header(&mut self, site: SiteId, state: &mut State) {
        self.cells.insert(site);
        let message = match state.cells.get(&site) {
            None => None,
            Some(CellPhase::Recorded) => {
                Some("the engine starts again on a cell whose result is recorded")
            }
            Some(_) => Some("a cell writes a second header"),
        };
        if let Some(message) = message {
            self.finding(
                "cell_finish_once",
                format!("{}: {message}", self.handler.sites[site]),
                Some("nested calls the finished cell made run a second time".to_owned()),
            );
        }
        state.cells.insert(site, CellPhase::Running);
    }

    fn cell_finished(&mut self, site: SiteId, state: &mut State) {
        match state.cells.get(&site) {
            Some(CellPhase::Running | CellPhase::Diverged) => {}
            _ => self.finding(
                "cell_finish_once",
                format!(
                    "{}: a finished update is appended to a cell that is not running",
                    self.handler.sites[site]
                ),
                None,
            ),
        }
        state.cells.insert(site, CellPhase::Finished);
    }

    fn site_result(&mut self, site: SiteId, state: &mut State) {
        let Some(phase) = state.cells.get(&site).copied() else {
            return;
        };
        if phase != CellPhase::Finished {
            self.finding(
                "cell_finish_once",
                format!(
                    "{}: the code site's result is appended without exactly one finished update before it",
                    self.handler.sites[site]
                ),
                None,
            );
        }
        state.cells.insert(site, CellPhase::Closed);
    }

    fn close_cell(&mut self, site: SiteId, state: &mut State) {
        match state.cells.remove(&site) {
            Some(CellPhase::Closed | CellPhase::Recorded) => {}
            _ => self.finding(
                "cell_finish_once",
                format!(
                    "{}: a path leaves the code site without a finished update and a site result",
                    self.handler.sites[site]
                ),
                None,
            ),
        }
    }

    fn sleep(&mut self, slot: Slot, what: &str, state: &State) {
        self.sleeps.insert(slot);
        if state.pending.is_empty() && state.durable_calls.contains(&slot) {
            return;
        }
        let pending = self.describe(&state.pending);
        self.finding(
            "durable_before_wait",
            format!("{what} waits while records are not durable: {pending}"),
            Some(
                "the host is evicted during the wait and the rerun repeats the model calls and tools before it"
                    .to_owned(),
            ),
        );
    }

    fn park(&mut self, slot: Slot, what: &str, state: &State) {
        self.parks.insert(slot);
        if state.phase != Phase::Running {
            self.finding(
                "finish_once",
                format!("{what} suspends a turn that already appended its status"),
                None,
            );
        }
        if state.pending.is_empty() && state.durable_calls.contains(&slot) {
            return;
        }
        let pending = self.describe(&state.pending);
        self.finding(
            "suspend_durable",
            format!("{what} ends the run waiting while records are not durable: {pending}"),
            Some(
                "the waiting status is lost with the process, so no resume can find the turn"
                    .to_owned(),
            ),
        );
    }

    fn wait(&mut self, site: SiteId, what: &str, state: &State) {
        self.waits.insert(site);
        if state.pending.contains(&Record::CellHeader(site)) {
            self.finding(
                "header_before_wait",
                format!("{what} waits on a timer while its header is not durable"),
                Some(
                    "the host dies during the wait, the header is lost and recovery asks the model to write the cell again"
                        .to_owned(),
                ),
            );
        }
    }

    fn run(
        &mut self,
        slot: Slot,
        class: EffectClass,
        outbound: bool,
        what: &str,
        state: &mut State,
    ) {
        let pending = self.describe(&state.pending);
        if outbound && self.plan.model.gate_before_outbound && !state.pending.is_empty() {
            self.finding(
                "output_gate",
                format!("{what} leaves the object before the output gate commits: {pending}"),
                None,
            );
        }
        if let Slot::Nested(site, _) = slot {
            self.nested_run(site, class, what, state);
        }
        if class == EffectClass::External {
            self.external(slot, what, state, &pending);
        }
        state.attempt = None;
    }

    fn nested_run(&mut self, site: SiteId, class: EffectClass, what: &str, state: &State) {
        let (property, message) = match state.cells.get(&site) {
            Some(CellPhase::Running) => return,
            Some(CellPhase::Diverged) => (
                "cell_replay_equal",
                format!(
                    "{} {what} is dispatched after its cell diverged from its journal",
                    class.name()
                ),
            ),
            _ => (
                "cell_finish_once",
                format!("{} {what} runs in a cell that is not running", class.name()),
            ),
        };
        self.finding(property, message, None);
    }

    fn external(&mut self, slot: Slot, what: &str, state: &State, pending: &str) {
        self.external_sites.insert(match slot {
            Slot::Site(site) => Slot::Site(self.handler.real[site]),
            Slot::Nested(..) => slot,
        });
        if !state.pending.is_empty() {
            self.finding(
                "durable_before_external",
                format!("external {what} runs while records are not durable: {pending}"),
                Some(format!(
                    "a crash right after the dispatch loses {pending}, so recovery cannot report the call unknown and may run it again"
                )),
            );
        } else if !state.durable_calls.contains(&slot) {
            self.finding(
                "durable_before_external",
                format!("external {what} runs before its call record is durable"),
                None,
            );
        }
        if state.recorded.contains(&slot) {
            self.finding(
                "external_once",
                format!("external {what} runs again although its result is journaled"),
                Some("a replay dispatches what the first run already dispatched".to_owned()),
            );
        }
        let recovery = self.plan.model.recovery;
        if state.reopened.contains(&slot) && recovery == Recovery::AtLoad {
            self.finding(
                "external_once",
                format!("external {what} runs again after recovery found its call open"),
                Some("the first run may have dispatched it before the crash".to_owned()),
            );
        }
        if let Recovery::InStep { .. } = recovery {
            self.attempt(slot, what, state);
        }
    }

    fn attempt(&mut self, slot: Slot, what: &str, state: &State) {
        match state.attempt {
            Some((attempted, true)) if attempted == slot => {}
            Some((attempted, false)) if attempted == slot => self.finding(
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

    fn yielded(&mut self, slot: Slot, class: EffectClass, seen: Seen, what: &str, state: &State) {
        let (unknown, known) = if slot.nested() {
            ("unknown_nested_reported", "nested_known")
        } else {
            ("unknown_reported", "idempotent_known")
        };
        match (class.may_be_unknown(), seen) {
            (true, Seen::Unknown) if !state.recovered => self.finding(
                unknown,
                format!("external {what} is reported unknown without a recovery record listing it"),
                None,
            ),
            (true, Seen::Unknown) | (false, Seen::Failed) => {}
            (true, Seen::Failed) => self.finding(
                unknown,
                format!("external {what}, reopened after a crash, is reported failed although it may have run"),
                None,
            ),
            (false, Seen::Unknown) => self.finding(
                known,
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

fn flush(state: &mut State, kept: Option<Record>) {
    let kept_call = match kept {
        Some(Record::Call(slot)) => Some(slot),
        _ => None,
    };
    let durable: Vec<Slot> = state
        .open_calls
        .iter()
        .copied()
        .filter(|slot| Some(*slot) != kept_call)
        .collect();
    state.durable_calls.extend(durable);
    if state.phase == Phase::StatusBuffered && state.pending.contains(&Record::Status) {
        state.phase = Phase::StatusDurable;
    }
    state.pending.retain(|record| Some(*record) == kept);
}

fn replaying(state: &State) -> bool {
    state.mode == Mode::Replay
}

fn cell_is(state: &State, site: SiteId, phase: CellPhase) -> bool {
    state.cells.get(&site) == Some(&phase)
}

fn enter(mut state: State, entry: Entry) -> Option<State> {
    match entry {
        Entry::Plain => {}
        Entry::Live => state.mode = Mode::Live,
        Entry::Rerun => state.mode = Mode::Replay,
        Entry::Replayed if !replaying(&state) => return None,
        Entry::Replayed => {}
        Entry::Reopened(_) | Entry::Recorded(_) if !replaying(&state) => return None,
        Entry::Reopened(slot) => {
            state.mode = Mode::Live;
            state.reopened.insert(slot);
            state.open_calls.insert(slot);
            state.durable_calls.insert(slot);
        }
        Entry::Recorded(slot) => {
            state.recorded.insert(slot);
        }
        Entry::ReopenedInBatch(site) => {
            if !replaying(&state) {
                return None;
            }
            state.reopened.insert(Slot::Site(site));
            state.open_calls.insert(Slot::Site(site));
            state.durable_calls.insert(Slot::Site(site));
        }
        Entry::Has(site) => {
            if !state.open_calls.contains(&Slot::Site(site)) {
                return None;
            }
        }
        Entry::FirstAttempt(slot) => state.attempt = Some((slot, true)),
        Entry::LaterAttempt(slot) => state.attempt = Some((slot, false)),
        Entry::CellRecorded(_) | Entry::CellResumed(_) if !replaying(&state) => return None,
        Entry::CellRecorded(site) => {
            state.cells.insert(site, CellPhase::Recorded);
        }
        Entry::CellResumed(site) => {
            state.cells.insert(site, CellPhase::Running);
            state.open_calls.insert(Slot::Site(site));
            state.durable_calls.insert(Slot::Site(site));
        }
        Entry::CellRunning(site) if !cell_is(&state, site, CellPhase::Running) => return None,
        Entry::CellRunning(_) => {}
        Entry::CellStopped(site) if !cell_is(&state, site, CellPhase::Finished) => return None,
        Entry::CellStopped(_) => {}
        Entry::Diverged(site) => {
            if !replaying(&state) || !cell_is(&state, site, CellPhase::Running) {
                return None;
            }
            state.cells.insert(site, CellPhase::Diverged);
        }
        Entry::HeaderPending(site) if !state.pending.contains(&Record::CellHeader(site)) => {
            return None;
        }
        Entry::HeaderDurable(site) if state.pending.contains(&Record::CellHeader(site)) => {
            return None;
        }
        Entry::HeaderPending(_) | Entry::HeaderDurable(_) => {}
    }
    Some(state)
}
