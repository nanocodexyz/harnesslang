use std::collections::BTreeSet;

use crate::ir::{Handler, Program, Type};
use crate::run::blob::Blobs;
use crate::run::engine::{
    End, Engine, Exit, Finish, Mode, Recovered, Setup, Suspension, ToolRequest, World,
    close_inbox, steer_count,
};
use crate::run::journal::Inbound;
use crate::run::journal::{Delivery, Output, Record, job_started, lost_delivery};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::session::{self, Session, SiteLog, TurnLog};
use crate::run::value::{self, Value};
use crate::target;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnRequest {
    pub turn: u64,
    pub handler: String,
    pub arguments: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ran {
    Finished(Finish),
    AlreadyFinished(Finish),
    Suspended(Suspension),
    NotDue(NotDue),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotDue {
    Busy,
    Unarmed,
    Early { due: u64, clock: u64 },
}

impl NotDue {
    pub fn describe(&self) -> String {
        match self {
            Self::Busy => "another turn is unfinished".to_owned(),
            Self::Unarmed => "no idle turn is armed".to_owned(),
            Self::Early { due, clock } => format!("the clock reads {clock}, before its due {due}"),
        }
    }
}

pub fn armed(program: &Program, state: &[Value]) -> Option<u64> {
    let idle = program.idle.as_ref()?;
    let due = state[idle.due].as_u64();
    let runs = state[idle.runs].as_u64();
    (due != 0 && runs < idle.bound.value).then_some(due)
}

pub fn idle_due(
    program: &Program,
    target: &target::Model,
    records: &[Record],
) -> Result<Option<u64>, Exit> {
    if program.idle.is_none() {
        return Ok(None);
    }
    let session = session::fold(records).map_err(corrupt)?;
    let next = match session.turns.last_key_value() {
        Some((_, log)) if log.finished.is_none() => return Ok(None),
        Some((last, _)) => last + 1,
        None => 1,
    };
    let state = state_before(program, target, &session, next)?;
    Ok(armed(program, &state))
}

fn idle_arguments(clock: u64, due: u64) -> Json {
    Json::object([
        (crate::ir::IDLE_AT, Json::Number(clock)),
        (crate::ir::IDLE_DUE, Json::Number(due)),
    ])
}

pub fn arguments_json(program: &Program, handler: &Handler, arguments: &[Value]) -> Json {
    let mut map = std::collections::BTreeMap::new();
    for (local, argument) in handler.params.iter().zip(arguments) {
        let param = &handler.locals[*local];
        map.insert(
            param.name.clone(),
            value::to_json(program, &param.ty, argument),
        );
    }
    Json::Object(map)
}

pub fn arguments_from_json(
    program: &Program,
    handler: &Handler,
    json: &Json,
) -> Result<Vec<Value>, String> {
    let mut arguments = Vec::new();
    for local in &handler.params {
        let param = &handler.locals[*local];
        let field = json
            .get(&param.name)
            .ok_or_else(|| format!("arguments lack '{}'", param.name))?;
        arguments.push(value::from_json(program, &param.ty, field)?);
    }
    Ok(arguments)
}

fn corrupt(error: String) -> Exit {
    Exit::Corrupt(error)
}

fn check_events(program: &Program, turn: u64, log: &TurnLog) -> Result<(), Exit> {
    let declared = &program.events;
    for event in &log.events {
        let taken = match &event.delivery.inbound {
            Inbound::Cancel => declared.cancel,
            Inbound::Steer(_) => declared.steers.is_some(),
            Inbound::Job(_) => declared.jobs.is_some(),
        };
        if !taken {
            return Err(corrupt(format!(
                "turn {turn} records a {} event, which the program does not take",
                event.delivery.inbound.name()
            )));
        }
    }
    let jobs = crate::run::engine::job_count(&log.events);
    if let Some(bound) = &declared.jobs
        && jobs > bound.value
    {
        return Err(corrupt(format!(
            "turn {turn} records {jobs} job notices, more than {} ({})",
            bound.name, bound.value
        )));
    }
    let steers = steer_count(&log.events);
    if let Some(bound) = &declared.steers
        && steers > bound.value
    {
        return Err(corrupt(format!(
            "turn {turn} records {steers} steers, more than {} ({})",
            bound.name, bound.value
        )));
    }
    Ok(())
}

pub fn replay(
    program: &Program,
    target: &target::Model,
    turn: u64,
    log: &TurnLog,
    state: Vec<Value>,
) -> Result<Vec<Value>, Exit> {
    replay_retiring(program, target, turn, log, state).map(|(state, _)| state)
}

pub fn replay_retiring(
    program: &Program,
    target: &target::Model,
    turn: u64,
    log: &TurnLog,
    state: Vec<Value>,
) -> Result<(Vec<Value>, u64), Exit> {
    let handler = program.handler(&log.handler).ok_or_else(|| {
        corrupt(format!(
            "turn {turn} ran handler '{}', which the program lacks",
            log.handler
        ))
    })?;
    let arguments = arguments_from_json(program, handler, &log.arguments).map_err(corrupt)?;
    check_events(program, turn, log)?;
    let finished = log
        .finished
        .as_ref()
        .ok_or_else(|| corrupt(format!("turn {turn} is replayed before it finished")))?;
    let setup = Setup {
        program,
        target,
        mutation: Mutation::None,
    };
    let ran = Engine::new(setup, handler, Mode::Replay, turn, state, log).run(&arguments)?;
    let End::Finished(finish) = &ran.end else {
        return Err(corrupt(format!(
            "replay of turn {turn} suspends, where the journal says it finished"
        )));
    };
    if finish.outcome != finished.outcome || finish.result != finished.result {
        return Err(corrupt(format!(
            "replay of turn {turn} finished {} where the journal says {}",
            finish.result.encode(),
            finished.result.encode()
        )));
    }
    Ok((ran.state, ran.retired))
}

pub fn state_before(
    program: &Program,
    target: &target::Model,
    session: &Session,
    turn: u64,
) -> Result<Vec<Value>, Exit> {
    let (start, mut state) = match session.checkpoints.range(..turn).next_back() {
        Some((revision, payload)) => {
            let payload = crate::run::json::decode(payload)
                .map_err(|error| corrupt(format!("checkpoint {revision}: {error}")))?;
            (
                revision + 1,
                value::state_from_json(program, &payload).map_err(corrupt)?,
            )
        }
        None => (1, entry_state(program, session)?),
    };
    for earlier in start..turn {
        let log = session
            .turns
            .get(&earlier)
            .ok_or_else(|| corrupt(format!("turn {earlier} is missing before turn {turn}")))?;
        state = replay(program, target, earlier, log, state)?;
    }
    Ok(state)
}

pub fn entry_state(program: &Program, session: &Session) -> Result<Vec<Value>, Exit> {
    let Some(payload) = session.checkpoints.get(&session::ENTRY_REVISION) else {
        return Ok(value::initial_state(program));
    };
    let payload = crate::run::json::decode(payload)
        .map_err(|error| corrupt(format!("the entry checkpoint: {error}")))?;
    value::state_from_json(program, &payload).map_err(corrupt)
}

pub fn replay_all(
    program: &Program,
    target: &target::Model,
    session: &Session,
) -> Result<Vec<Value>, Exit> {
    let mut state = entry_state(program, session)?;
    for (turn, log) in &session.turns {
        if log.finished.is_some() {
            state = replay(program, target, *turn, log, state)?;
        }
    }
    Ok(state)
}

#[derive(Default)]
struct Recovering {
    unknown: Vec<u64>,
    replied: Vec<(u64, Output)>,
    undispatched: Vec<u64>,
}

fn recover(
    program: &Program,
    world: &mut dyn World,
    turn: u64,
    log: &mut TurnLog,
    mutation: Mutation,
) -> Recovering {
    let open = log.open_seqs(|class| match mutation {
        Mutation::RecoveryRedispatchesExternal | Mutation::RecoverySkipped => false,
        Mutation::IdempotentReportedUnknown => true,
        _ => class.may_be_unknown(),
    });
    let mut recovering = Recovering::default();
    for seq in open {
        let site = log.sites.get_mut(&seq).expect("an open seq has a site");
        let SiteLog::Open(call) = site else {
            panic!("open_seqs returns open sites");
        };
        let recovered = if call.effect.may_be_unknown() && program.recoverable(&call.tool) {
            ask_recover(program, world, turn, seq, call)
        } else {
            Recovered::Unknown
        };
        let recovered = match recovered {
            Recovered::Unknown
                if mutation == Mutation::RecoverTrustsLost && program.recoverable(&call.tool) =>
            {
                Recovered::NotDispatched
            }
            recovered => recovered,
        };
        match recovered {
            Recovered::Unknown => {
                *site = SiteLog::Unknown(call.clone());
                recovering.unknown.push(seq);
            }
            Recovered::NotDispatched => recovering.undispatched.push(seq),
            Recovered::Reply(output) => {
                *site = SiteLog::Done(call.clone(), output.clone());
                recovering.replied.push((seq, output));
            }
        }
    }
    recovering
}

fn ask_recover(
    program: &Program,
    world: &mut dyn World,
    turn: u64,
    seq: u64,
    call: &crate::run::session::Call,
) -> Recovered {
    let returns = Type::Text;
    let recovered = world.recover(&ToolRequest {
        turn,
        seq,
        tool: &call.tool,
        effect: call.effect,
        input: &call.input,
        returns: &returns,
        nested: None,
        admitted: None,
        job: false,
    });
    match recovered {
        Recovered::Reply(output)
            if job_started(&output).is_some() && program.jobs_of(&call.tool).is_some() =>
        {
            Recovered::Reply(output)
        }
        Recovered::Reply(output) => {
            Recovered::Reply(value::checked_output(program, &call.tool, &returns, output))
        }
        other => other,
    }
}

fn lost_jobs(
    program: &Program,
    world: &mut dyn World,
    turn: u64,
    session: &crate::run::session::Session,
    mutation: Mutation,
) -> Vec<Delivery> {
    if program.events.jobs.is_none() || session.jobs.is_empty() {
        return Vec::new();
    }
    let open: Vec<String> = session.jobs.keys().cloned().collect();
    let lost = world.jobs(turn, &open);
    if mutation == Mutation::JobLostDropped {
        return Vec::new();
    }
    let lost = if mutation == Mutation::JobLostAll {
        open
    } else {
        lost
    };
    lost.iter()
        .filter(|key| session.jobs.contains_key(*key))
        .map(|key| lost_delivery(key))
        .collect()
}

fn recover_nested(log: &mut TurnLog, mutation: Mutation) -> Vec<(u64, u64)> {
    if matches!(
        mutation,
        Mutation::CellRecoveryRedispatchesExternal | Mutation::RecoverySkipped
    ) {
        return Vec::new();
    }
    let unknown = log.open_nested(|class| class.may_be_unknown());
    for id in &unknown {
        let site = log
            .nested
            .get_mut(id)
            .expect("an open nested call has a site");
        let SiteLog::Open(call) = site else {
            panic!("open_nested returns open calls");
        };
        *site = SiteLog::Unknown(call.clone());
    }
    unknown
}

fn assume_nested_failed(log: &mut TurnLog, ids: Vec<(u64, u64)>) -> Vec<((u64, u64), String)> {
    let mut assumed = Vec::new();
    for id in ids {
        let site = log
            .nested
            .get_mut(&id)
            .expect("a recovered nested call has a site");
        let SiteLog::Unknown(call) = site else {
            panic!("recover_nested marks open calls unknown");
        };
        let error = format!("{} was lost in a crash", call.tool);
        *site = SiteLog::Done(call.clone(), Output::Failed(error.clone()));
        assumed.push((id, error));
    }
    assumed
}

fn assume_failed(log: &mut TurnLog, seqs: Vec<u64>) -> Vec<(u64, String)> {
    let mut assumed = Vec::new();
    for seq in seqs {
        let site = log.sites.get_mut(&seq).expect("a recovered seq has a site");
        let SiteLog::Unknown(call) = site else {
            panic!("recover marks open calls unknown");
        };
        let error = format!("{} was lost in a crash", call.tool);
        *site = SiteLog::Done(call.clone(), Output::Failed(error.clone()));
        assumed.push((seq, error));
    }
    assumed
}

pub fn run_turn(
    program: &Program,
    target: &target::Model,
    mutation: Mutation,
    world: &mut dyn World,
    epoch: u64,
    records: &[Record],
    request: &TurnRequest,
) -> Result<Ran, Exit> {
    let session = session::fold(records).map_err(corrupt)?;
    let handler = program
        .handler(&request.handler)
        .expect("a turn request names a handler of the program");
    let arguments = arguments_json(program, handler, &request.arguments);
    if let Some(log) = session.turns.get(&request.turn)
        && let Some(finished) = &log.finished
    {
        if program.events.any() {
            close_inbox(world, request.turn, finished.last, &log.events);
        }
        return Ok(Ran::AlreadyFinished(Finish {
            turn: request.turn,
            outcome: finished.outcome,
            result: finished.result.clone(),
        }));
    }
    let idles = program.idles(&request.handler);
    let fresh = !session.turns.contains_key(&request.turn);
    if idles
        && fresh
        && session
            .turns
            .last_key_value()
            .is_some_and(|(_, log)| log.finished.is_none())
    {
        return Ok(Ran::NotDue(NotDue::Busy));
    }
    let state = state_before(program, target, &session, request.turn)?;
    let blobs = Blobs::of_session(program, &session).map_err(corrupt)?;
    blobs.check_state(program, &state).map_err(corrupt)?;
    let mut pending = Vec::new();
    let mut recovering = Recovering::default();
    let mut assumed = Vec::new();
    let mut nested = Vec::new();
    let mut nested_assumed = Vec::new();
    let log = match session.turns.get(&request.turn) {
        Some(log) => {
            if log.handler != request.handler || (!idles && log.arguments != arguments) {
                return Err(corrupt(format!(
                    "turn {} was admitted with other arguments",
                    request.turn
                )));
            }
            check_events(program, request.turn, log)?;
            let mut log = log.clone();
            if matches!(target.recovery, target::Recovery::AtLoad) {
                recovering = recover(program, world, request.turn, &mut log, mutation);
                nested = recover_nested(&mut log, mutation);
            }
            if mutation == Mutation::CellRecoveryReportsFailed {
                nested_assumed = assume_nested_failed(&mut log, std::mem::take(&mut nested));
            }
            if mutation == Mutation::RecoveryReportsFailed {
                assumed = assume_failed(&mut log, std::mem::take(&mut recovering.unknown));
            }
            log
        }
        None => {
            let next = session
                .turns
                .last_key_value()
                .map_or(1, |(last, _)| last + 1);
            if request.turn != next {
                return Err(corrupt(format!(
                    "turn {} is requested when turn {next} is next",
                    request.turn
                )));
            }
            let arguments = if idles {
                let Some(due) = armed(program, &state) else {
                    return Ok(Ran::NotDue(NotDue::Unarmed));
                };
                let clock = world.clock();
                if clock < due && mutation != Mutation::IdleEarly {
                    return Ok(Ran::NotDue(NotDue::Early { due, clock }));
                }
                idle_arguments(clock, due)
            } else {
                arguments
            };
            pending.push(Record::Admitted {
                turn: request.turn,
                handler: request.handler.clone(),
                arguments,
            });
            TurnLog::new(request.handler.clone(), Json::Null)
        }
    };
    let setup = Setup {
        program,
        target,
        mutation,
    };
    let snapshot = (!program.sides.is_empty()).then(|| state.clone());
    let lost = lost_jobs(program, world, request.turn, &session, mutation);
    let mut jobs = session.jobs.clone();
    for (seq, output) in &recovering.replied {
        if job_started(output).is_some()
            && let Some(SiteLog::Done(call, _)) = log.sites.get(seq)
        {
            jobs.insert(crate::ir::spawn_key(request.turn, *seq), call.tool.clone());
        }
    }
    let attested: BTreeSet<u64> = recovering.undispatched.iter().copied().collect();
    let mut engine = Engine::new(
        setup,
        handler,
        Mode::Live { world, epoch },
        request.turn,
        state,
        &log,
    )
    .with_next_cell(session.next_cell())
    .with_blobs(blobs)
    .with_jobs(jobs, lost)
    .with_attested(attested);
    if let Some(snapshot) = snapshot {
        engine = engine.with_snapshot(snapshot);
    }
    for record in pending {
        engine.append(record);
    }
    let attested = !recovering.replied.is_empty() || !recovering.undispatched.is_empty();
    if !recovering.unknown.is_empty() || !nested.is_empty() || attested {
        let attempt = engine.next_attempt();
        engine.append(Record::Recovery {
            turn: request.turn,
            attempt,
            unknown: recovering.unknown,
            nested,
            replied: recovering.replied.iter().map(|(seq, _)| *seq).collect(),
            undispatched: recovering.undispatched,
        });
    }
    for (seq, output) in recovering.replied {
        engine.append(Record::Result {
            turn: request.turn,
            seq,
            output,
        });
    }
    for ((seq, inner), error) in nested_assumed {
        engine.append(Record::NestedResult {
            turn: request.turn,
            seq,
            nested: inner,
            output: Output::Failed(error),
        });
    }
    for (seq, error) in assumed {
        engine.append(Record::Result {
            turn: request.turn,
            seq,
            output: Output::Failed(error),
        });
    }
    engine.run(&request.arguments).map(|ran| match ran.end {
        End::Finished(finish) => Ran::Finished(finish),
        End::Suspended(suspension) => Ran::Suspended(suspension),
    })
}
