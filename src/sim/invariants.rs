use std::collections::{BTreeMap, BTreeSet};

use crate::effect::EffectClass;
use crate::ir::Program;
use crate::run::cell::{CellOutcome, encode_output};
use crate::run::cell_journal::{Entry, unknown_outcome};
use crate::run::engine::Finish;
use crate::run::journal::{self, Output, Record, call_id, nested_id};
use crate::run::json::Json;
use crate::run::session;
use crate::run::session::{Session, SiteLog};
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
    NestedExternalOnce,
    UnknownNestedReported,
    NestedKnown,
    CellFinishesOnce,
    CellReplayEqual,
    HeaderBeforeWait,
    CellBounded,
    EnumValid,
    NestedEnumValid,
    NoTrap,
    InvariantKept,
    WorstCase,
    SequentialEqual,
}

impl Invariant {
    pub const ALL: [Self; 21] = [
        Self::ExternalOnce,
        Self::SeqUnique,
        Self::UnknownReported,
        Self::IdempotentKnown,
        Self::FinishOnce,
        Self::Liveness,
        Self::ReplayEqual,
        Self::Panic,
        Self::NestedExternalOnce,
        Self::UnknownNestedReported,
        Self::NestedKnown,
        Self::CellFinishesOnce,
        Self::CellReplayEqual,
        Self::HeaderBeforeWait,
        Self::CellBounded,
        Self::EnumValid,
        Self::NestedEnumValid,
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
            Self::NestedExternalOnce => "nested_external_once",
            Self::UnknownNestedReported => "unknown_nested_reported",
            Self::NestedKnown => "nested_known",
            Self::CellFinishesOnce => "cell_finishes_once",
            Self::CellReplayEqual => "cell_replay_equal",
            Self::HeaderBeforeWait => "header_before_wait",
            Self::CellBounded => "cell_bounded",
            Self::EnumValid => "enum_valid",
            Self::NestedEnumValid => "nested_enum_valid",
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
    nested_external: BTreeSet<String>,
    nested_results: BTreeMap<String, Vec<Output>>,
    nested_unknown: BTreeSet<String>,
}

fn seq_unique(records: &[Record]) -> Result<Calls, Violation> {
    let mut calls = Calls::default();
    let mut last: BTreeMap<u64, u64> = BTreeMap::new();
    let mut last_result: BTreeMap<u64, u64> = BTreeMap::new();
    let mut open: BTreeSet<String> = BTreeSet::new();
    let mut nested_last: BTreeMap<(u64, u64), u64> = BTreeMap::new();
    for record in records {
        match record {
            Record::NestedCall {
                turn, seq, nested, ..
            } => {
                let previous = nested_last.get(&(*turn, *seq)).copied().unwrap_or(0);
                let id = nested_id(*turn, *seq, *nested);
                if *nested <= previous {
                    return violation(
                        Invariant::SeqUnique,
                        format!(
                            "nested call {id} is recorded after nested call {previous} of its cell site"
                        ),
                    );
                }
                nested_last.insert((*turn, *seq), *nested);
                open.insert(id.clone());
                calls.nested_external.insert(id);
            }
            Record::NestedResult {
                turn,
                seq,
                nested,
                output,
            } => {
                let id = nested_id(*turn, *seq, *nested);
                if !open.remove(&id) {
                    return violation(
                        Invariant::SeqUnique,
                        format!("result for nested call {id}, which has no open call"),
                    );
                }
                calls
                    .nested_results
                    .entry(id)
                    .or_default()
                    .push(output.clone());
            }
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
            Record::Recovery {
                turn,
                unknown,
                nested,
                ..
            } => {
                for (seq, inner) in nested {
                    let id = nested_id(*turn, *seq, *inner);
                    if !open.remove(&id) {
                        return violation(
                            Invariant::SeqUnique,
                            format!("recovery reports nested call {id}, which has no open call"),
                        );
                    }
                    calls.nested_unknown.insert(id);
                }
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
    let session = session::fold(records).expect("liveness folded this journal");
    cells(program, &calls, &session)
}

fn nested_dispatches(world: &SimWorld<'_>) -> Result<(), Violation> {
    if let Some(fault) = world.cell_faults.first() {
        return Err(fault.clone());
    }
    for (id, dispatched) in &world.nested_dispatched {
        if dispatched.external > 1 {
            return violation(
                Invariant::NestedExternalOnce,
                format!(
                    "nested external call {id} ({}) was dispatched {} times",
                    dispatched.tool, dispatched.external
                ),
            );
        }
    }
    Ok(())
}

fn nested_reported(world: &SimWorld<'_>, calls: &Calls) -> Result<(), Violation> {
    for (id, dispatched) in &world.nested_dispatched {
        if let Some(results) = calls.nested_results.get(id)
            && dispatched.external > 0
            && results.first() != dispatched.outputs.first()
        {
            return violation(
                Invariant::UnknownNestedReported,
                format!(
                    "nested external call {id} records {results:?}, the tool returned {:?}",
                    dispatched.outputs
                ),
            );
        }
    }
    for id in &calls.nested_external {
        let known = calls.nested_results.contains_key(id);
        let unknown = calls.nested_unknown.contains(id);
        let dispatches = world
            .nested_dispatched
            .get(id)
            .map_or(0, |dispatched| dispatched.external);
        if known && dispatches == 0 {
            return violation(
                Invariant::UnknownNestedReported,
                format!(
                    "nested external call {id} records a result although it was never dispatched"
                ),
            );
        }
        if known && unknown {
            return violation(
                Invariant::UnknownNestedReported,
                format!("nested external call {id} is both recorded and reported unknown"),
            );
        }
    }
    Ok(())
}

fn tool_class(program: &Program, name: &str) -> EffectClass {
    program.dispatch_class(name)
}

fn cells(program: &Program, calls: &Calls, session: &Session) -> Result<(), Violation> {
    let unknown_encoded = unknown_outcome().encode();
    for (turn, log) in &session.turns {
        for (seq, cell) in &log.cells {
            let site_unknown = match log.sites.get(seq) {
                Some(SiteLog::Done(_, output)) => {
                    matches!(
                        CellOutcome::from_output(output),
                        Ok(CellOutcome::Unknown(_))
                    )
                }
                _ => {
                    return violation(
                        Invariant::CellFinishesOnce,
                        format!(
                            "cell {} of finished turn {turn} has no site result",
                            cell.cell
                        ),
                    );
                }
            };
            if !cell.finished {
                return violation(
                    Invariant::CellFinishesOnce,
                    format!(
                        "cell {} of finished turn {turn} has no finished update",
                        cell.cell
                    ),
                );
            }
            let mut names = BTreeMap::new();
            let mut results = BTreeMap::new();
            for entry in &cell.entries {
                match entry {
                    Entry::Call { id, name, .. } => {
                        names.insert(*id, name.clone());
                    }
                    Entry::Result {
                        id,
                        encoded,
                        success,
                    } => {
                        results.insert(*id, (encoded.clone(), *success));
                    }
                    _ => {}
                }
            }
            for (id, name) in &names {
                let nested = nested_id(*turn, *seq, *id);
                let class = tool_class(program, name);
                let result = results.get(id);
                let delivered_unknown = result
                    .is_some_and(|(encoded, success)| !success && *encoded == unknown_encoded);
                if class == EffectClass::External {
                    let listed = calls.nested_unknown.contains(&nested);
                    if delivered_unknown != listed && (result.is_some() || !listed) {
                        return violation(
                            Invariant::UnknownNestedReported,
                            format!(
                                "nested external call {nested} is listed unknown: {listed}, delivered the unknown outcome: {delivered_unknown}"
                            ),
                        );
                    }
                    continue;
                }
                if site_unknown {
                    continue;
                }
                if result.is_none() || delivered_unknown {
                    return violation(
                        Invariant::NestedKnown,
                        format!(
                            "{} nested call {nested} ends without a known result",
                            class.name()
                        ),
                    );
                }
            }
        }
    }
    Ok(())
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
    nested_dispatches(world)?;
    let calls = seq_unique(records)?;
    nested_reported(world, &calls)?;
    enum_valid(world, records)?;
    nested_enum_valid(world, records)?;
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
        if known && dispatches == 0 {
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

fn canonical_key(record: &Record) -> (u64, u8, u64, u8) {
    match record {
        Record::Admitted { turn, .. } => (*turn, 0, 0, 0),
        Record::Recovery { turn, attempt, .. } => {
            (*turn, 1, 0, u8::try_from(*attempt).unwrap_or(u8::MAX))
        }
        Record::Call { turn, seq, .. } => (*turn, 2, *seq, 0),
        Record::Result { turn, seq, .. } | Record::Model { turn, seq, .. } => (*turn, 2, *seq, 1),
        Record::Status { turn, .. } => (*turn, 3, 0, 0),
        Record::Checkpoint { revision, .. } => (*revision, 4, 0, 0),
        Record::NestedCall { .. } | Record::NestedResult { .. } | Record::Cell { .. } => {
            panic!("the checker refuses parallel for in a harness with a code tool")
        }
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

fn nested_enum_valid(world: &SimWorld<'_>, records: &[Record]) -> Result<(), Violation> {
    if world
        .nested_dispatched
        .values()
        .all(|dispatched| dispatched.values.is_empty())
    {
        return Ok(());
    }
    let Ok(folded) = session::fold(records) else {
        return Ok(());
    };
    for (turn, log) in &folded.turns {
        for (seq, cell) in &log.cells {
            nested_results_valid(world, *turn, *seq, &cell.entries)?;
        }
    }
    Ok(())
}

fn nested_results_valid(
    world: &SimWorld<'_>,
    turn: u64,
    seq: u64,
    entries: &[Entry],
) -> Result<(), Violation> {
    let unknown = unknown_outcome().encode();
    let mut calls: BTreeMap<u64, (&str, &Json)> = BTreeMap::new();
    for entry in entries {
        match entry {
            Entry::Call { id, name, input } => {
                calls.insert(*id, (name, input));
            }
            Entry::Result {
                id,
                encoded,
                success,
            } => {
                let nested = nested_id(turn, seq, *id);
                let (Some(dispatched), Some(&(tool, input))) =
                    (world.nested_dispatched.get(&nested), calls.get(id))
                else {
                    continue;
                };
                let returned: Vec<_> = dispatched
                    .values
                    .iter()
                    .filter(|returned| returned.tool == tool && &returned.input == input)
                    .collect();
                let recorded = (encoded.clone(), *success);
                if *encoded == unknown
                    || returned.is_empty()
                    || returned
                        .iter()
                        .any(|returned| encode_output(&returned.expected) == recorded)
                {
                    continue;
                }
                let outputs: Vec<_> = returned.iter().map(|returned| &returned.output).collect();
                return violation(
                    Invariant::NestedEnumValid,
                    format!(
                        "nested call {nested} ({tool}) records {encoded}, but the tool returned {outputs:?}: a result outside its text enum must be recorded as failed"
                    ),
                );
            }
            _ => {}
        }
    }
    Ok(())
}
