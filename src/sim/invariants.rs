use std::collections::{BTreeMap, BTreeSet};

use crate::ir::Program;
use crate::run::journal::{Output, Record, call_id};
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
    NoTrap,
    InvariantKept,
}

impl Invariant {
    pub const ALL: [Self; 10] = [
        Self::ExternalOnce,
        Self::SeqUnique,
        Self::UnknownReported,
        Self::IdempotentKnown,
        Self::FinishOnce,
        Self::Liveness,
        Self::ReplayEqual,
        Self::Panic,
        Self::NoTrap,
        Self::InvariantKept,
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
            Self::NoTrap => "no_trap",
            Self::InvariantKept => "invariant_kept",
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
    liveness_and_replay(program, world, turns, records)
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
    let calls = seq_unique(records)?;
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
