use std::collections::{BTreeMap, BTreeSet};

use crate::effect::EffectClass;
use crate::ir::{ListingId, Program};
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
    EventOnce,
    EventKept,
    EventBeforeRead,
    EventReplayEqual,
    ToolAdmitted,
    NoTrap,
    InvariantKept,
    WorstCase,
    CheckpointCompacted,
    SequentialEqual,
    SuspendDurable,
    ResumeMatched,
    WaitUntil,
    WallLimit,
    ModeSent,
    BlobJournaled,
    SearchListed,
    DeniedUnrun,
    ChildScoped,
    FamilyBounded,
    SideIsolated,
    IdleDue,
    InterruptHonored,
    JobOnce,
    JobReported,
    ForkStateEqual,
}

impl Invariant {
    pub const ALL: [Self; 43] = [
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
        Self::EventOnce,
        Self::EventKept,
        Self::EventBeforeRead,
        Self::EventReplayEqual,
        Self::ToolAdmitted,
        Self::NoTrap,
        Self::InvariantKept,
        Self::WorstCase,
        Self::CheckpointCompacted,
        Self::SequentialEqual,
        Self::SuspendDurable,
        Self::ResumeMatched,
        Self::WaitUntil,
        Self::WallLimit,
        Self::ModeSent,
        Self::BlobJournaled,
        Self::SearchListed,
        Self::DeniedUnrun,
        Self::ChildScoped,
        Self::FamilyBounded,
        Self::SideIsolated,
        Self::IdleDue,
        Self::InterruptHonored,
        Self::JobOnce,
        Self::JobReported,
        Self::ForkStateEqual,
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
            Self::EventOnce => "event_once",
            Self::EventKept => "event_kept",
            Self::EventBeforeRead => "event_before_read",
            Self::EventReplayEqual => "event_replay_equal",
            Self::ToolAdmitted => "tool_admitted",
            Self::NoTrap => "no_trap",
            Self::InvariantKept => "invariant_kept",
            Self::WorstCase => "worst_case",
            Self::CheckpointCompacted => "checkpoint_compacted",
            Self::SequentialEqual => "sequential_equal",
            Self::SuspendDurable => "suspend_durable",
            Self::ResumeMatched => "resume_matched",
            Self::WaitUntil => "wait_until",
            Self::WallLimit => "wall_limit",
            Self::ModeSent => "mode_sent",
            Self::BlobJournaled => "blob_journaled",
            Self::SearchListed => "search_listed",
            Self::DeniedUnrun => "denied_unrun",
            Self::ChildScoped => "child_scoped",
            Self::FamilyBounded => "family_bounded",
            Self::SideIsolated => "side_isolated",
            Self::IdleDue => "idle_due",
            Self::InterruptHonored => "interrupt_honored",
            Self::JobOnce => "job_once",
            Self::JobReported => "job_reported",
            Self::ForkStateEqual => "fork_state_equal",
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

pub fn interrupted_count(program: &Program, records: &[Record]) -> u64 {
    let mut count = 0;
    for record in records {
        match record {
            Record::Model { response, .. }
                if *response == crate::run::engine::interrupted_response() =>
            {
                count += 1;
            }
            Record::Result {
                output: Output::Failed(error),
                ..
            } if error == crate::ir::INTERRUPTED_REASON && program.any_interruptible() => {
                count += 1;
            }
            _ => {}
        }
    }
    count
}

struct Arrived {
    inbound: journal::Inbound,
    at: u64,
}

fn interrupt_honored(program: &Program, records: &[Record]) -> Result<(), Violation> {
    if !program.any_interruptible() {
        return Ok(());
    }
    let mut events: BTreeMap<u64, Vec<Arrived>> = BTreeMap::new();
    let mut tools: BTreeMap<(u64, u64), String> = BTreeMap::new();
    let mut replied: BTreeSet<(u64, u64)> = BTreeSet::new();
    for record in records {
        let (turn, seq, interrupted, interrupts) = match record {
            Record::Event {
                turn, at, delivery, ..
            } => {
                events.entry(*turn).or_default().push(Arrived {
                    inbound: delivery.inbound.clone(),
                    at: *at,
                });
                continue;
            }
            Record::Call {
                turn, seq, tool, ..
            } => {
                tools.insert((*turn, *seq), tool.clone());
                continue;
            }
            Record::Model {
                turn,
                seq,
                response,
                ..
            } => (
                *turn,
                *seq,
                *response == crate::run::engine::interrupted_response(),
                program.interrupts(crate::ir::SiteKind::Model, ""),
            ),
            Record::Result { turn, seq, output } => {
                let tool = tools.get(&(*turn, *seq)).cloned().unwrap_or_default();
                if replied.remove(&(*turn, *seq)) {
                    continue;
                }
                let reason = matches!(output, Output::Failed(error) if error == crate::ir::INTERRUPTED_REASON);
                (
                    *turn,
                    *seq,
                    reason,
                    program.interrupts(crate::ir::SiteKind::Dispatch, &tool),
                )
            }
            Record::Recovery {
                turn,
                replied: attested,
                undispatched,
                ..
            } => {
                for seq in attested.iter().chain(undispatched) {
                    replied.insert((*turn, *seq));
                }
                continue;
            }
            _ => continue,
        };
        if !interrupts.any() {
            continue;
        }
        let arrived = events.get(&turn).map_or(&[][..], Vec::as_slice);
        let caused = arrived
            .iter()
            .any(|event| event.inbound.interrupts(interrupts));
        let required = arrived.iter().any(|event| {
            event.inbound.interrupts(interrupts)
                && (event.at >= seq || event.inbound == journal::Inbound::Cancel)
        });
        let id = call_id(turn, seq);
        if required && !interrupted {
            return violation(
                Invariant::InterruptHonored,
                format!(
                    "{id} is interruptible by {} and an event that interrupts it was recorded before its outcome, yet it records a finished outcome",
                    interrupts.describe()
                ),
            );
        }
        if interrupted && !caused {
            return violation(
                Invariant::InterruptHonored,
                format!(
                    "{id} records an interrupted outcome, yet no event that interrupts it was recorded"
                ),
            );
        }
    }
    Ok(())
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
    checkpoint_compacted(program, world, records)?;
    liveness_and_replay(program, world, turns, records)?;
    world.inbox.kept(records)?;
    crate::sim::idle::check(program, world, records)?;
    world.jobs.check(program, records)?;
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

pub fn checkpoint_compacted(
    program: &Program,
    world: &SimWorld<'_>,
    records: &[Record],
) -> Result<(), Violation> {
    let Ok(session) = session::fold(records) else {
        return Ok(());
    };
    let Ok(mut state) = turn::entry_state(program, &session) else {
        return Ok(());
    };
    for (turn, log) in &session.turns {
        if log.finished.is_none() {
            continue;
        }
        let Ok((replayed, retired)) =
            turn::replay_retiring(program, &world.target, *turn, log, state)
        else {
            return Ok(());
        };
        state = replayed;
        if let Some(payload) = session.checkpoints.get(turn)
            && let Ok(held) = crate::run::json::decode(payload)
        {
            let expected = value::state_json(program, &state);
            if held != expected {
                return violation(
                    Invariant::CheckpointCompacted,
                    format!(
                        "checkpoint {turn} holds {} where replaying the journal to turn {turn} gives {}: a checkpoint must hold the lists as clear and keep left them",
                        field_difference(program, &held, &expected, true),
                        field_difference(program, &held, &expected, false)
                    ),
                );
            }
        }
        if retired == 0 {
            continue;
        }
        let unsealed = world
            .finish_flushes
            .iter()
            .any(|flush| flush.turn == *turn && !flush.checkpointed);
        if unsealed {
            return violation(
                Invariant::CheckpointCompacted,
                format!(
                    "turn {turn} retired {retired} list entries but flushed its status without its checkpoint, so the next load replays the retired prefix from an earlier checkpoint"
                ),
            );
        }
    }
    Ok(())
}

fn sessions_and_denials(world: &SimWorld<'_>, records: &[Record]) -> Result<(), Violation> {
    for (key, child) in &world.children {
        if let Err(failed) = &child.result {
            return violation(
                failed.invariant,
                format!("child session {key}: {}", failed.detail),
            );
        }
    }
    if let Some(detail) = world.denied_ran.first() {
        return violation(Invariant::DeniedUnrun, detail.clone());
    }
    let mut spawned: BTreeMap<(u64, u64), String> = BTreeMap::new();
    for record in records {
        match record {
            Record::Call {
                turn, seq, tool, ..
            } if world.program.spawn_of(tool).is_some() => {
                spawned.insert((*turn, *seq), tool.clone());
            }
            Record::Result {
                turn,
                seq,
                output: Output::Ok(Json::String(key)),
            } if spawned.contains_key(&(*turn, *seq)) => {
                let scoped = crate::ir::scoped_spawn_key(&world.scope, *turn, *seq);
                if *key != scoped || !world.children.contains_key(key) {
                    return violation(
                        Invariant::ChildScoped,
                        format!(
                            "spawn {} at turn {turn} seq {seq} records child key {key:?}, but its call starts the child session {scoped:?}",
                            spawned[&(*turn, *seq)]
                        ),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn family_bounded(world: &SimWorld<'_>, records: &[Record]) -> Result<(), Violation> {
    if world.children.is_empty() {
        return Ok(());
    }
    let program = world.program;
    let mut started: BTreeMap<u64, u64> = BTreeMap::new();
    for (key, child) in &world.children {
        let declared = program
            .spawn_of(&child.tool)
            .and_then(|spawn| spawn.sessions.as_ref());
        if let Some(limit) = declared
            && child.sessions > limit.value
        {
            return violation(
                Invariant::FamilyBounded,
                format!(
                    "spawn {} started child session {key:?} and {} more below it, over sessions at most {} ({})",
                    child.tool,
                    child.sessions - 1,
                    limit.name,
                    limit.value
                ),
            );
        }
        let count = started.entry(child.turn).or_insert(0);
        *count = count
            .checked_add(child.sessions)
            .expect("a family's sessions fit u64, bounded by child_sessions_per_seed_max");
    }
    for (turn, count) in started {
        let Some(handler) = records.iter().find_map(|record| match record {
            Record::Admitted {
                turn: admitted,
                handler,
                ..
            } if *admitted == turn => program.handler(handler),
            _ => None,
        }) else {
            continue;
        };
        let worst = &handler.worst;
        let levels = if world.mutation == crate::run::mutation::Mutation::FamilyOneLevel {
            crate::ir::worst::Levels::One
        } else {
            crate::ir::worst::Levels::Tree
        };
        let bound = crate::ir::worst::family_with(program, [0; 3], &worst.spawns, levels)
            .expect("hl check counted the family in u64")
            .map_or(0, |family| family.sessions);
        if count > bound {
            return violation(
                Invariant::FamilyBounded,
                format!(
                    "turn {turn} ({}) started {count} sessions in its tree, over its worst case of {bound} ({} child sessions, {})",
                    handler.name,
                    worst.spawns.total,
                    worst.spawns.describe_loops()
                ),
            );
        }
    }
    Ok(())
}

pub fn safety(world: &SimWorld<'_>, records: &[Record]) -> Result<Calls, Violation> {
    world.side_fault()?;
    sessions_and_denials(world, records)?;
    family_bounded(world, records)?;
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
    crate::sim::park::check(world, records)?;
    world.inbox.safety(records)?;
    let calls = seq_unique(records)?;
    nested_reported(world, &calls)?;
    enum_valid(world, records)?;
    nested_enum_valid(world, records)?;
    let refused = tool_admitted(world, records)?;
    mode_sent(world, records)?;
    blob_journaled(world.program, records)?;
    search_listed(world.program, records)?;
    interrupt_honored(world.program, records)?;
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
            && results.first().cloned().map(crate::run::blob::digested)
                != dispatched.outputs.first().cloned()
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
        let preempted = calls.results.get(id).is_some_and(|results| {
            results.first() == Some(&Output::Failed(crate::ir::INTERRUPTED_REASON.to_owned()))
        });
        if known && dispatches == 0 && !refused.contains(id) && !preempted {
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

fn field_difference(program: &Program, held: &Json, expected: &Json, first: bool) -> String {
    let differing = program
        .state
        .iter()
        .find(|field| held.get(&field.name) != expected.get(&field.name));
    let Some(field) = differing else {
        return if first {
            held.encode()
        } else {
            expected.encode()
        };
    };
    let value = if first { held } else { expected }.get(&field.name);
    match value {
        Some(Json::Array(items)) => format!("{} entries of {}", items.len(), field.name),
        Some(other) => format!("{} = {}", field.name, other.encode()),
        None => format!("no {}", field.name),
    }
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

fn blob_journaled(program: &Program, records: &[Record]) -> Result<(), Violation> {
    if program.blobs.is_none() {
        return Ok(());
    }
    let mut tools: BTreeMap<String, &str> = BTreeMap::new();
    let mut blobs = crate::run::blob::Blobs::default();
    for record in records {
        match record {
            Record::Call {
                turn, seq, tool, ..
            } => {
                tools.insert(call_id(*turn, *seq), tool);
            }
            Record::Result {
                turn,
                seq,
                output: Output::Ok(json @ Json::Object(_)),
            } => {
                let id = call_id(*turn, *seq);
                let blob = tools
                    .get(&id)
                    .is_some_and(|tool| crate::run::blob::returned_bound(program, tool).is_some());
                if !blob {
                    continue;
                }
                if let Err(error) = blobs.read(json) {
                    return violation(
                        Invariant::BlobJournaled,
                        format!("result {id} ({}): {error}", tools[&id]),
                    );
                }
            }
            Record::Checkpoint {
                revision, payload, ..
            } => {
                let Ok(state) = crate::run::json::decode(payload)
                    .map_err(|error| error.to_string())
                    .and_then(|json| value::state_from_json(program, &json))
                else {
                    continue;
                };
                if let Err(error) = blobs.check_state(program, &state) {
                    return violation(
                        Invariant::BlobJournaled,
                        format!("checkpoint {revision}: {error}"),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn search_listed(program: &Program, records: &[Record]) -> Result<(), Violation> {
    if !program.tools.iter().any(|tool| tool.finds.is_some()) {
        return Ok(());
    }
    let (calls, admissions, results) = admissions(program, records);
    for (id, output) in results {
        let (Some(call), Output::Ok(Json::String(text))) = (calls.get(&id), output) else {
            continue;
        };
        let Some(listing) = program.search_of(call.tool) else {
            continue;
        };
        for found in value::found_names(text) {
            let listed = program.listed(found).filter(|(id, _)| *id == listing);
            let admitted =
                listed.and_then(|(_, name)| admitted_fingerprint(&admissions, call, listing, name));
            if admitted.is_none() {
                return violation(
                    Invariant::SearchListed,
                    format!(
                        "search {id} ({}) recorded {found:?} as found, which the latest read of listing {} in turn {} did not list",
                        call.tool, program.listings[listing].name, call.turn
                    ),
                );
            }
        }
    }
    Ok(())
}

fn discovered_before(
    program: &Program,
    calls: &BTreeMap<String, Recorded<'_>>,
    results: &[(String, &Output)],
    call: &Recorded<'_>,
    tool: &str,
) -> bool {
    results.iter().any(|(id, output)| {
        let (Some(search), Output::Ok(Json::String(text))) = (calls.get(id), output) else {
            return false;
        };
        search.turn == call.turn
            && search.seq < call.seq
            && program.search_of(search.tool).is_some()
            && value::found_names(text).contains(&tool)
    })
}

fn mode_named(mode: Option<&str>) -> String {
    mode.map_or_else(|| "no mode".to_owned(), |mode| format!("mode {mode}"))
}

fn mode_sent(world: &SimWorld<'_>, records: &[Record]) -> Result<(), Violation> {
    for record in records {
        let Record::Model {
            turn,
            seq,
            response,
            mode,
        } = record
        else {
            continue;
        };
        if *response == crate::run::engine::interrupted_response() {
            continue;
        }
        let answered: Vec<&Option<String>> = world
            .model_modes
            .get(&(*turn, *seq))
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter(|(_, reply)| reply == response)
            .map(|(sent, _)| sent)
            .collect();
        let Some(sent) = answered
            .iter()
            .find(|sent| **sent == mode)
            .or(answered.last())
        else {
            return violation(
                Invariant::ModeSent,
                format!(
                    "model call {} is journaled with a reply no request returned",
                    call_id(*turn, *seq)
                ),
            );
        };
        if *sent != mode {
            return violation(
                Invariant::ModeSent,
                format!(
                    "model call {} is journaled with {} but its reply answered a request sent with {}",
                    call_id(*turn, *seq),
                    mode_named(mode.as_deref()),
                    mode_named(sent.as_deref())
                ),
            );
        }
    }
    Ok(())
}

fn tool_admitted(world: &SimWorld<'_>, records: &[Record]) -> Result<BTreeSet<String>, Violation> {
    let program = world.program;
    let (calls, admissions, results) = admissions(program, records);
    for (id, executions) in &world.executed {
        for executed in executions {
            let input = match program.fork_of(&executed.tool) {
                Some(at) => crate::run::fork::request_of(at, &executed.input),
                None => executed.input.clone(),
            };
            if let Some(params) = program.input_params(&executed.tool)
                && let Err(error) = value::input_fits(program, params, &input)
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
        if program.deferred(listing)
            && !discovered_before(program, &calls, &results, call, &last.tool)
        {
            return violation(
                Invariant::ToolAdmitted,
                format!(
                    "listed call {id} ({}) ran although no search in turn {} found it: listing {} is deferred",
                    last.tool, call.turn, program.listings[listing].name
                ),
            );
        }
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
    let results_all = results.clone();
    for (id, output) in results {
        let (Output::Failed(error), Some(call)) = (output, calls.get(&id)) else {
            continue;
        };
        let invalid = match call.input {
            Json::String(raw) => match value::tool_input(program, call.tool, raw) {
                value::Input::Invalid(invalid) => Some(invalid),
                value::Input::Untyped | value::Input::Typed(_) => None,
            },
            input => program.fork_of(call.tool).and_then(|at| {
                input.get(crate::ir::FORK_STATE).is_none().then(|| {
                    crate::ir::fork_missing(crate::run::fork::requested(at, input).unwrap_or(0))
                })
            }),
        };
        let expected = invalid.or_else(|| {
            let (listing, name) = program.listed(call.tool)?;
            let listing_name = &program.listings[listing].name;
            if admitted_fingerprint(admissions, call, listing, name).is_none() {
                return Some(value::not_admitted(call.tool, listing_name, name));
            }
            (program.deferred(listing)
                && !discovered_before(program, calls, &results_all, call, call.tool))
            .then(|| value::not_discovered(call.tool, listing_name))
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
        Record::Host { .. } => (0, 0, 0, 0),
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
