use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{ListingId, Program};
use crate::run::engine::Finish;
use crate::run::journal::{self, Output, Record, call_id};
use crate::run::json::Json;
use crate::run::session;
use crate::run::turn;
use crate::run::value;
use crate::sim::world::SimWorld;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Invariant {
    ExternalOnce,
    SeqUnique,
    UnknownReported,
    IdempotentKnown,
    FinishOnce,
    Liveness,
    ReplayEqual,
    Panic,
    EnumValid,
    EventOnce,
    EventKept,
    EventBeforeRead,
    EventReplayEqual,
    ToolAdmitted,
    NoTrap,
    InvariantKept,
    WorstCase,
    SequentialEqual,
}

impl Invariant {
    pub const ALL: [Self; 18] = [
        Self::ExternalOnce,
        Self::SeqUnique,
        Self::UnknownReported,
        Self::IdempotentKnown,
        Self::FinishOnce,
        Self::Liveness,
        Self::ReplayEqual,
        Self::Panic,
        Self::EnumValid,
        Self::EventOnce,
        Self::EventKept,
        Self::EventBeforeRead,
        Self::EventReplayEqual,
        Self::ToolAdmitted,
        Self::NoTrap,
        Self::InvariantKept,
        Self::WorstCase,
        Self::SequentialEqual,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::ExternalOnce => "external_once",
            Self::SeqUnique => "seq_unique",
            Self::UnknownReported => "unknown_reported",
            Self::IdempotentKnown => "idempotent_known",
            Self::FinishOnce => "finish_once",
            Self::Liveness => "liveness",
            Self::ReplayEqual => "replay_equal",
            Self::Panic => "panic",
            Self::EnumValid => "enum_valid",
            Self::EventOnce => "event_once",
            Self::EventKept => "event_kept",
            Self::EventBeforeRead => "event_before_read",
            Self::EventReplayEqual => "event_replay_equal",
            Self::ToolAdmitted => "tool_admitted",
            Self::NoTrap => "no_trap",
            Self::InvariantKept => "invariant_kept",
            Self::WorstCase => "worst_case",
            Self::SequentialEqual => "sequential_equal",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub invariant: Invariant,
    pub detail: String,
}

fn violation<T>(invariant: Invariant, detail: String) -> Result<T, Violation> {
    Err(Violation { invariant, detail })
}

#[derive(Default)]
pub struct Calls {
    external: BTreeSet<String>,
    other: BTreeSet<String>,
    results: BTreeMap<String, Vec<Output>>,
    unknown: BTreeSet<String>,
    statuses: BTreeMap<u64, Vec<Record>>,
}

fn seq_unique(records: &[Record]) -> Result<Calls, Violation> {
    let mut calls = Calls::default();
    let mut last: BTreeMap<u64, u64> = BTreeMap::new();
    let mut last_result: BTreeMap<u64, u64> = BTreeMap::new();
    let mut open: BTreeSet<String> = BTreeSet::new();
    for record in records {
        match record {
            Record::Call {
                turn, seq, effect, ..
            } => {
                follows(&mut last, *turn, *seq)?;
                let id = call_id(*turn, *seq);
                open.insert(id.clone());
                if effect.may_be_unknown() {
                    calls.external.insert(id);
                } else {
                    calls.other.insert(id);
                }
            }
            Record::Model { turn, seq, .. } => follows(&mut last, *turn, *seq)?,
            Record::Result { turn, seq, output } => {
                let id = call_id(*turn, *seq);
                if !open.remove(&id) {
                    return violation(
                        Invariant::SeqUnique,
                        format!("result for {id}, which has no open call"),
                    );
                }
                let previous = last_result.insert(*turn, *seq).unwrap_or(0);
                if *seq <= previous {
                    return violation(
                        Invariant::SequentialEqual,
                        format!(
                            "turn {turn} records the result of seq {seq} after the result of seq {previous}, where one call at a time records them in order"
                        ),
                    );
                }
                calls.results.entry(id).or_default().push(output.clone());
            }
            Record::Recovery { turn, unknown, .. } => {
                for seq in unknown {
                    let id = call_id(*turn, *seq);
                    if !open.remove(&id) {
                        return violation(
                            Invariant::SeqUnique,
                            format!("recovery reports {id}, which has no open call"),
                        );
                    }
                    calls.unknown.insert(id);
                }
            }
            Record::Status { turn, .. } => calls
                .statuses
                .entry(*turn)
                .or_default()
                .push(record.clone()),
            _ => {}
        }
    }
    Ok(calls)
}

fn follows(last: &mut BTreeMap<u64, u64>, turn: u64, seq: u64) -> Result<(), Violation> {
    let previous = last.get(&turn).copied().unwrap_or(0);
    if seq <= previous {
        return violation(
            Invariant::SeqUnique,
            format!("turn {turn} records seq {seq} after seq {previous}"),
        );
    }
    last.insert(turn, seq);
    Ok(())
}

pub fn check(
    program: &Program,
    world: &SimWorld<'_>,
    turns: u64,
    records: &[Record],
) -> Result<(), Violation> {
    let calls = safety(world, records)?;
    finish_once(world, &calls, turns)?;
    idempotent_known(&calls, records)?;
    liveness_and_replay(program, world, turns, records)?;
    world.inbox.kept(records)
}

pub fn safety(world: &SimWorld<'_>, records: &[Record]) -> Result<Calls, Violation> {
    for (id, dispatched) in &world.dispatched {
        if dispatched.external > 1 {
            return violation(
                Invariant::ExternalOnce,
                format!(
                    "external call {id} ({}) was dispatched {} times",
                    dispatched.tool, dispatched.external
                ),
            );
        }
    }
    world.inbox.safety(records)?;
    let calls = seq_unique(records)?;
    enum_valid(world, records)?;
    let refused = tool_admitted(world, records)?;
    for (id, dispatched) in world
        .dispatched
        .iter()
        .filter(|(_, dispatched)| dispatched.external > 0)
    {
        if !calls.external.contains(id) {
            return violation(
                Invariant::UnknownReported,
                format!(
                    "external call {id} ({}) was dispatched but the journal has no durable call record for it",
                    dispatched.tool
                ),
            );
        }
        if let Some(results) = calls.results.get(id)
            && results.first() != dispatched.outputs.first()
        {
            return violation(
                Invariant::UnknownReported,
                format!(
                    "external call {id} records {results:?}, the tool returned {:?}",
                    dispatched.outputs
                ),
            );
        }
    }
    for id in &calls.external {
        let known = calls.results.contains_key(id);
        let unknown = calls.unknown.contains(id);
        let dispatches = world
            .dispatched
            .get(id)
            .map_or(0, |dispatched| dispatched.external);
        if known && dispatches == 0 && !refused.contains(id) {
            return violation(
                Invariant::UnknownReported,
                format!("external call {id} records a result although it was never dispatched"),
            );
        }
        if known && unknown {
            return violation(
                Invariant::UnknownReported,
                format!("external call {id} is both recorded and reported unknown"),
            );
        }
    }
    for id in &calls.other {
        if calls.unknown.contains(id) {
            return violation(
                Invariant::IdempotentKnown,
                format!("none or idempotent call {id} is reported unknown"),
            );
        }
    }
    if let Some(fault) = world.ack_faults.first() {
        return violation(Invariant::FinishOnce, fault.clone());
    }
    for (turn, statuses) in &calls.statuses {
        if statuses.len() > 1 {
            return violation(
                Invariant::FinishOnce,
                format!("turn {turn} has {} status records", statuses.len()),
            );
        }
    }
    Ok(calls)
}

fn enum_valid(world: &SimWorld<'_>, records: &[Record]) -> Result<(), Violation> {
    let mut calls: BTreeMap<String, (&str, &Json)> = BTreeMap::new();
    for record in records {
        match record {
            Record::Call {
                turn,
                seq,
                tool,
                input,
                ..
            } => {
                calls.insert(call_id(*turn, *seq), (tool, input));
            }
            Record::Result { turn, seq, output } => {
                let id = call_id(*turn, *seq);
                let (Some(dispatched), Some(&(tool, input))) =
                    (world.dispatched.get(&id), calls.get(&id))
                else {
                    continue;
                };
                let returned: Vec<_> = dispatched
                    .values
                    .iter()
                    .filter(|returned| returned.tool == tool && &returned.input == input)
                    .collect();
                if returned.is_empty()
                    || returned.iter().any(|returned| &returned.expected == output)
                {
                    continue;
                }
                let outputs: Vec<_> = returned.iter().map(|returned| &returned.output).collect();
                return violation(
                    Invariant::EnumValid,
                    format!(
                        "call {id} ({tool}) records {output:?}, but the tool returned {outputs:?}: a result outside its text enum must be recorded as failed"
                    ),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

type Admissions = BTreeMap<(u64, ListingId), BTreeMap<u64, Option<BTreeMap<String, String>>>>;

struct Recorded<'r> {
    turn: u64,
    seq: u64,
    tool: &'r str,
    input: &'r Json,
}

fn admissions<'r>(
    program: &Program,
    records: &'r [Record],
) -> (
    BTreeMap<String, Recorded<'r>>,
    Admissions,
    Vec<(String, &'r Output)>,
) {
    let mut calls = BTreeMap::new();
    let mut admissions: Admissions = BTreeMap::new();
    let mut results = Vec::new();
    for record in records {
        match record {
            Record::Call {
                turn,
                seq,
                tool,
                input,
                ..
            } => {
                let recorded = Recorded {
                    turn: *turn,
                    seq: *seq,
                    tool,
                    input,
                };
                calls.insert(call_id(*turn, *seq), recorded);
            }
            Record::Result { turn, seq, output } => {
                let id = call_id(*turn, *seq);
                results.push((id.clone(), output));
                let Some(call) = calls.get(&id) else { continue };
                let listing = program
                    .listings
                    .iter()
                    .position(|listing| listing.name == call.tool);
                let Some(listing) = listing.filter(|_| *call.input == Json::Null) else {
                    continue;
                };
                let tools = match output {
                    Output::Ok(json) => value::admitted(json).ok(),
                    Output::Failed(_) => None,
                };
                admissions
                    .entry((*turn, listing))
                    .or_default()
                    .insert(*seq, tools);
            }
            _ => {}
        }
    }
    (calls, admissions, results)
}

fn admitted_fingerprint<'a>(
    admissions: &'a Admissions,
    call: &Recorded<'_>,
    listing: ListingId,
    name: &str,
) -> Option<&'a String> {
    admissions
        .get(&(call.turn, listing))?
        .range(..call.seq)
        .next_back()?
        .1
        .as_ref()?
        .get(name)
}

fn tool_admitted(world: &SimWorld<'_>, records: &[Record]) -> Result<BTreeSet<String>, Violation> {
    let program = world.program;
    let (calls, admissions, results) = admissions(program, records);
    for (id, executions) in &world.executed {
        for executed in executions {
            if let Some(params) = program.input_params(&executed.tool)
                && let Err(error) = value::input_fits(program, params, &executed.input)
            {
                return violation(
                    Invariant::ToolAdmitted,
                    format!(
                        "call {id} ({}) ran with input {}, which does not fit its declared input: {error}",
                        executed.tool,
                        executed.input.encode()
                    ),
                );
            }
        }
        let last = executions
            .last()
            .expect("an executed call ran at least once");
        let Some((listing, name)) = program.listed(&last.tool) else {
            continue;
        };
        let Some(call) = calls.get(id).filter(|call| call.tool == last.tool) else {
            continue;
        };
        let admitted = admitted_fingerprint(&admissions, call, listing, name);
        if admitted != last.fingerprint.as_ref() {
            return violation(
                Invariant::ToolAdmitted,
                format!(
                    "listed call {id} ({}) ran as {:?} while listing {} admitted {:?} in turn {}: a tool that changed since admission must be refused",
                    last.tool,
                    last.fingerprint,
                    program.listings[listing].name,
                    admitted,
                    call.turn
                ),
            );
        }
    }
    Ok(refused(program, &calls, &admissions, results))
}

fn refused(
    program: &Program,
    calls: &BTreeMap<String, Recorded<'_>>,
    admissions: &Admissions,
    results: Vec<(String, &Output)>,
) -> BTreeSet<String> {
    let mut refused = BTreeSet::new();
    for (id, output) in results {
        let (Output::Failed(error), Some(call)) = (output, calls.get(&id)) else {
            continue;
        };
        let invalid = match call.input {
            Json::String(raw) => match value::tool_input(program, call.tool, raw) {
                value::Input::Invalid(invalid) => Some(invalid),
                value::Input::Untyped | value::Input::Typed(_) => None,
            },
            _ => None,
        };
        let expected = invalid.or_else(|| {
            let (listing, name) = program.listed(call.tool)?;
            admitted_fingerprint(admissions, call, listing, name)
                .is_none()
                .then(|| value::not_admitted(call.tool, &program.listings[listing].name, name))
        });
        if expected.as_ref() == Some(error) {
            refused.insert(id);
        }
    }
    refused
}

fn canonical_key(record: &Record) -> (u64, u8, u64, u8) {
    match record {
        Record::Admitted { turn, .. } => (*turn, 0, 0, 0),
        Record::Recovery { turn, attempt, .. } => {
            (*turn, 1, 0, u8::try_from(*attempt).unwrap_or(u8::MAX))
        }
        Record::Call { turn, seq, .. } => (*turn, 2, *seq, 0),
        Record::Result { turn, seq, .. } | Record::Model { turn, seq, .. } => (*turn, 2, *seq, 1),
        Record::Event { turn, at, .. } => (*turn, 2, *at, 2),
        Record::Status { turn, .. } => (*turn, 3, 0, 0),
        Record::Checkpoint { revision, .. } => (*revision, 4, 0, 0),
    }
}

pub fn sequential_equal(
    journal: &[Record],
    acks: &[Finish],
    sequential: &[Record],
    sequential_acks: &[Finish],
) -> Result<(), Violation> {
    let mut canonical = journal.to_vec();
    canonical.sort_by_key(canonical_key);
    if acks != sequential_acks {
        return violation(
            Invariant::SequentialEqual,
            format!(
                "on a clean run the turns finish {acks:?}, where one call at a time finishes {sequential_acks:?}"
            ),
        );
    }
    let differs = canonical
        .iter()
        .zip(sequential)
        .position(|(left, right)| left != right);
    let at = match differs {
        Some(at) => at,
        None if canonical.len() == sequential.len() => return Ok(()),
        None => canonical.len().min(sequential.len()),
    };
    let shown = |records: &[Record]| {
        records.get(at).map_or_else(
            || "nothing".to_owned(),
            |record| journal::to_json(record).encode(),
        )
    };
    violation(
        Invariant::SequentialEqual,
        format!(
            "on a clean run the journal, read per sequence number, has {} at record {at}, where one call at a time writes {}",
            shown(&canonical),
            shown(sequential)
        ),
    )
}

pub fn trapped_in_body(acks: &[Finish]) -> bool {
    acks.iter().any(|finish| {
        finish
            .result
            .get("trap")
            .and_then(Json::as_str)
            .is_some_and(|trap| !trap.starts_with("requires(") && !trap.starts_with("model reply"))
    })
}

fn finish_once(world: &SimWorld<'_>, calls: &Calls, turns: u64) -> Result<(), Violation> {
    for turn in 1..=turns {
        let acks: Vec<_> = world.acks.iter().filter(|ack| ack.turn == turn).collect();
        if acks.is_empty() && calls.statuses.contains_key(&turn) {
            return violation(
                Invariant::FinishOnce,
                format!("turn {turn} finished but was never acknowledged"),
            );
        }
        if acks.windows(2).any(|pair| pair[0] != pair[1]) {
            return violation(
                Invariant::FinishOnce,
                format!("turn {turn} was acknowledged with different outcomes"),
            );
        }
    }
    Ok(())
}

fn idempotent_known(calls: &Calls, records: &[Record]) -> Result<(), Violation> {
    let finished: BTreeSet<u64> = calls.statuses.keys().copied().collect();
    for record in records {
        let Record::Call {
            turn, seq, effect, ..
        } = record
        else {
            continue;
        };
        let id = call_id(*turn, *seq);
        if !effect.may_be_unknown() && finished.contains(turn) && !calls.results.contains_key(&id) {
            return violation(
                Invariant::IdempotentKnown,
                format!(
                    "{} call {id} has no known result in finished turn {turn}",
                    effect.name()
                ),
            );
        }
    }
    Ok(())
}

fn liveness_and_replay(
    program: &Program,
    world: &SimWorld<'_>,
    turns: u64,
    records: &[Record],
) -> Result<(), Violation> {
    if let Some(fault) = world.owner_faults.first() {
        return violation(Invariant::Liveness, fault.clone());
    }
    let session = session::fold(records).map_err(|error| Violation {
        invariant: Invariant::Liveness,
        detail: format!("journal does not fold: {error}"),
    })?;
    for turn in 1..=turns {
        let finished = session
            .turns
            .get(&turn)
            .is_some_and(|log| log.finished.is_some());
        if !finished {
            return violation(Invariant::Liveness, format!("turn {turn} did not finish"));
        }
    }
    let failed = |exit| Violation {
        invariant: Invariant::ReplayEqual,
        detail: format!("replaying the journal failed: {exit:?}"),
    };
    let replayed = turn::replay_all(program, &world.target, &session).map_err(failed)?;
    let resumed =
        turn::state_before(program, &world.target, &session, turns + 1).map_err(failed)?;
    let replayed = value::state_json(program, &replayed);
    let resumed = value::state_json(program, &resumed);
    if replayed != resumed {
        return violation(
            Invariant::ReplayEqual,
            format!(
                "replaying the whole journal gives state {} but resuming from its checkpoints gives {}",
                replayed.encode(),
                resumed.encode()
            ),
        );
    }
    Ok(())
}
