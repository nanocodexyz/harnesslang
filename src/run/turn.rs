use crate::ir::{Handler, Program};
use crate::run::engine::{Engine, Exit, Finish, Mode, World};
use crate::run::journal::Record;
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
}

pub fn arguments_json(program: &Program, handler: &Handler, arguments: &[Value]) -> Json {
    let mut map = std::collections::BTreeMap::new();
    for (local, argument) in handler.params.iter().zip(arguments) {
        let param = &handler.locals[*local];
        map.insert(param.name.clone(), value::to_json(program, &param.ty, argument));
    }
    Json::Object(map)
}

pub fn arguments_from_json(program: &Program, handler: &Handler, json: &Json) -> Result<Vec<Value>, String> {
    let mut arguments = Vec::new();
    for local in &handler.params {
        let param = &handler.locals[*local];
        let field = json.get(&param.name).ok_or_else(|| format!("arguments lack '{}'", param.name))?;
        arguments.push(value::from_json(program, &param.ty, field)?);
    }
    Ok(arguments)
}

fn corrupt(error: String) -> Exit {
    Exit::Corrupt(error)
}

pub fn replay(program: &Program, target: &target::Model, turn: u64, log: &TurnLog, state: Vec<Value>) -> Result<Vec<Value>, Exit> {
    let handler = program.handler(&log.handler).ok_or_else(|| corrupt(format!("turn {turn} ran handler '{}', which the program lacks", log.handler)))?;
    let arguments = arguments_from_json(program, handler, &log.arguments).map_err(corrupt)?;
    let finished = log.finished.as_ref().ok_or_else(|| corrupt(format!("turn {turn} is replayed before it finished")))?;
    let ran = Engine::new(program, handler, target, Mutation::None, Mode::Replay, turn, state, log).run(&arguments)?;
    if ran.finish.outcome != finished.outcome || ran.finish.result != finished.result {
        return Err(corrupt(format!("replay of turn {turn} finished {} where the journal says {}", ran.finish.result.encode(), finished.result.encode())));
    }
    Ok(ran.state)
}

pub fn state_before(program: &Program, target: &target::Model, session: &Session, turn: u64) -> Result<Vec<Value>, Exit> {
    let (start, mut state) = match session.checkpoints.range(..turn).next_back() {
        Some((revision, payload)) => (revision + 1, value::state_from_json(program, payload).map_err(corrupt)?),
        None => (1, value::initial_state(program)),
    };
    for earlier in start..turn {
        let log = session.turns.get(&earlier).ok_or_else(|| corrupt(format!("turn {earlier} is missing before turn {turn}")))?;
        state = replay(program, target, earlier, log, state)?;
    }
    Ok(state)
}

pub fn replay_all(program: &Program, target: &target::Model, session: &Session) -> Result<Vec<Value>, Exit> {
    let mut state = value::initial_state(program);
    for (turn, log) in &session.turns {
        if log.finished.is_some() {
            state = replay(program, target, *turn, log, state)?;
        }
    }
    Ok(state)
}

fn recover(log: &mut TurnLog, mutation: Mutation) -> Vec<u64> {
    let unknown = log.open_seqs(|class| match mutation {
        Mutation::RecoveryRedispatchesExternal => false,
        Mutation::IdempotentReportedUnknown => true,
        _ => class.may_be_unknown(),
    });
    for seq in &unknown {
        let site = log.sites.get_mut(seq).expect("an open seq has a site");
        let SiteLog::Open(call) = site else {
            panic!("open_seqs returns open sites");
        };
        *site = SiteLog::Unknown(call.clone());
    }
    unknown
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
    let handler = program.handler(&request.handler).expect("a turn request names a handler of the program");
    let arguments = arguments_json(program, handler, &request.arguments);
    if let Some(log) = session.turns.get(&request.turn)
        && let Some(finished) = &log.finished
    {
        return Ok(Ran::AlreadyFinished(Finish { turn: request.turn, outcome: finished.outcome, result: finished.result.clone() }));
    }
    let state = state_before(program, target, &session, request.turn)?;
    let mut pending = Vec::new();
    let mut unknown = Vec::new();
    let log = match session.turns.get(&request.turn) {
        Some(log) => {
            if log.handler != request.handler || log.arguments != arguments {
                return Err(corrupt(format!("turn {} was admitted with other arguments", request.turn)));
            }
            let mut log = log.clone();
            if matches!(target.recovery, target::Recovery::AtLoad) {
                unknown = recover(&mut log, mutation);
            }
            log
        }
        None => {
            let next = session.turns.last_key_value().map_or(1, |(last, _)| last + 1);
            if request.turn != next {
                return Err(corrupt(format!("turn {} is requested when turn {next} is next", request.turn)));
            }
            pending.push(Record::Admitted { turn: request.turn, handler: request.handler.clone(), arguments });
            TurnLog { handler: request.handler.clone(), arguments: Json::Null, sites: Default::default(), attempts: 0, finished: None }
        }
    };
    let mut engine = Engine::new(program, handler, target, mutation, Mode::Live { world, epoch }, request.turn, state, &log);
    for record in pending {
        engine.append(record);
    }
    if !unknown.is_empty() {
        let attempt = engine.next_attempt();
        engine.append(Record::Recovery { turn: request.turn, attempt, unknown });
    }
    engine.run(&request.arguments).map(|ran| Ran::Finished(ran.finish))
}
