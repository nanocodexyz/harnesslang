use crate::ir::{self, ForkAt, Program};
use crate::run::engine::{Exit, Setup};
use crate::run::journal::Record;
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::session::{self, Session};
use crate::run::side::{last_finished, run_hook};
use crate::run::turn::state_before;
use crate::run::value::{self, Value};
use crate::target;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    pub turn: u64,
    pub discarded: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    Missing(u64),
    Corrupt(String),
}

impl Refused {
    pub fn describe(&self) -> String {
        match self {
            Self::Missing(turn) => ir::fork_missing(*turn),
            Self::Corrupt(error) => error.clone(),
        }
    }
}

pub fn select(session: &Session, at: Option<u64>) -> Result<Selected, u64> {
    let turn = at.unwrap_or_else(|| last_finished(session));
    let finished = |turn: &u64| {
        session
            .turns
            .get(turn)
            .is_some_and(|log| log.finished.is_some())
    };
    if turn != session::ENTRY_REVISION && !finished(&turn) {
        return Err(turn);
    }
    let after = turn.checked_add(1).ok_or(turn)?;
    let discarded = session
        .turns
        .range(after..)
        .filter(|(_, log)| log.finished.is_some())
        .count();
    Ok(Selected {
        turn,
        discarded: discarded as u64,
    })
}

pub fn requested(at: ForkAt, input: &Json) -> Option<u64> {
    match at {
        ForkAt::Latest => None,
        ForkAt::Turn => input.get(ir::FORK_TURN).and_then(Json::as_u64),
    }
}

pub fn request_of(at: ForkAt, input: &Json) -> Json {
    if input.get(ir::FORK_STATE).is_none() {
        return input.clone();
    }
    match at {
        ForkAt::Latest => Json::object([]),
        ForkAt::Turn => Json::object([(
            ir::FORK_TURN,
            input.get(ir::FORK_TURN).cloned().unwrap_or(Json::Null),
        )]),
    }
}

pub fn entry_payload(
    setup: Setup<'_>,
    session: &Session,
    selected: &Selected,
    origin: &str,
    live: Option<&[Value]>,
) -> Result<String, Exit> {
    let state = match (setup.mutation, live) {
        (Mutation::ForkLiveState, Some(live)) => live.to_vec(),
        _ => state_before(setup.program, setup.target, session, selected.turn + 1)?,
    };
    let state = match &setup.program.forked {
        Some(hook) if setup.mutation != Mutation::ForkHookSkipped => run_hook(
            setup,
            hook,
            state,
            &[
                Value::Text(origin.to_owned()),
                Value::U64(selected.discarded),
            ],
        )?,
        _ => state,
    };
    Ok(value::state_json(setup.program, &state).encode())
}

pub fn input(selected: &Selected, origin: &str, payload: String) -> Json {
    Json::object([
        (ir::FORK_DISCARDED, Json::Number(selected.discarded)),
        (ir::FORK_ORIGIN, Json::text(origin)),
        (ir::FORK_STATE, Json::String(payload)),
        (ir::FORK_TURN, Json::Number(selected.turn)),
    ])
}

pub fn records_of(program: &Program, input: &Json) -> Result<[Record; 2], String> {
    let number = |key: &str| {
        input
            .get(key)
            .and_then(Json::as_u64)
            .ok_or_else(|| format!("a fork's input lacks '{key}'"))
    };
    let turn = number(ir::FORK_TURN)?;
    let discarded = number(ir::FORK_DISCARDED)?;
    let Some(Json::String(origin)) = input.get(ir::FORK_ORIGIN) else {
        return Err(format!("a fork's input lacks '{}'", ir::FORK_ORIGIN));
    };
    let Some(Json::String(payload)) = input.get(ir::FORK_STATE) else {
        return Err(format!("a fork's input lacks '{}'", ir::FORK_STATE));
    };
    Ok([
        Record::Host {
            key: ir::FORKED_HOST_KEY.to_owned(),
            value: Json::object([
                (ir::FORK_DISCARDED, Json::Number(discarded)),
                (ir::FORK_ORIGIN, Json::text(origin)),
                (ir::FORK_TURN, Json::Number(turn)),
            ]),
        },
        Record::Checkpoint {
            revision: session::ENTRY_REVISION,
            state: program.name.clone(),
            payload: payload.clone(),
        },
    ])
}

pub fn fork_entry(
    program: &Program,
    target: &target::Model,
    records: &[Record],
    at: Option<u64>,
    origin: &str,
) -> Result<[Record; 2], Refused> {
    let session = session::fold(records).map_err(Refused::Corrupt)?;
    let selected = select(&session, at).map_err(Refused::Missing)?;
    let setup = Setup {
        program,
        target,
        mutation: Mutation::None,
    };
    let payload = entry_payload(setup, &session, &selected, origin, None).map_err(|exit| {
        Refused::Corrupt(match exit {
            Exit::Corrupt(error) => error,
            Exit::Stop(stop) => format!("the fork stopped: {stop:?}"),
        })
    })?;
    records_of(program, &input(&selected, origin, payload)).map_err(Refused::Corrupt)
}
