
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkAt {
    Latest,
    Turn,
}

pub const ENTRY_REVISION: u64 = 0;
pub const FORKED_HOST_KEY: &str = "forked";
pub const FORK_TURN: &str = "turn";
pub const FORK_ORIGIN: &str = "origin";
pub const FORK_DISCARDED: &str = "discarded";
pub const FORK_STATE: &str = "state";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForkRefused {
    Missing(u64),
    Corrupt(String),
}

impl ForkRefused {
    pub fn describe(&self) -> String {
        match self {
            Self::Missing(turn) => fork_missing(*turn),
            Self::Corrupt(error) => error.clone(),
        }
    }
}

pub fn fork_missing(turn: u64) -> String {
    format!("no finished turn {turn} to fork from")
}

pub fn fork_select(session: &Session, at: Option<u64>) -> Result<(u64, u64), u64> {
    let turn = at.unwrap_or_else(|| last_finished(session));
    let finished = session.turns.get(&turn).is_some_and(|log| log.finished.is_some());
    let entry = turn == ENTRY_REVISION && session.start.first_turn() == FIRST_TURN;
    let checkpointed = session.checkpoints.contains_key(&turn) && turn + 1 == session.start.first_turn();
    if !finished && !entry && !checkpointed {
        return Err(turn);
    }
    let after = turn.checked_add(1).ok_or(turn)?;
    let discarded = session.turns.range(after..).filter(|(_, log)| log.finished.is_some()).count();
    Ok((turn, discarded as u64))
}

pub fn fork_requested(at: ForkAt, input: &Json) -> Option<u64> {
    match at {
        ForkAt::Latest => None,
        ForkAt::Turn => input.get(FORK_TURN).and_then(Json::as_u64),
    }
}

pub fn fork_request_of(at: ForkAt, input: &Json) -> Json {
    if input.get(FORK_STATE).is_none() {
        return input.clone();
    }
    match at {
        ForkAt::Latest => Json::object(vec![]),
        ForkAt::Turn => Json::object(vec![(FORK_TURN, input.get(FORK_TURN).cloned().unwrap_or(Json::Null))]),
    }
}

pub fn fork_payload(session: &Session, turn: u64, discarded: u64, origin: &str) -> Result<String, Exit> {
    let mut state = state_before(session, turn + 1)?;
    program::run_forked(&mut state, origin.to_owned(), discarded).map_err(|halt| match halt {
        Halt::Exit(exit) => exit,
        Halt::Trap(trap) => panic!("the prover keeps on forked from trapping: {trap}"),
        Halt::Suspend(_) => panic!("on forked never suspends"),
    })?;
    Ok(program::state_json(&state).encode())
}

pub fn fork_input(turn: u64, discarded: u64, origin: &str, payload: String) -> Json {
    Json::object(vec![
        (FORK_DISCARDED, Json::Number(discarded)),
        (FORK_ORIGIN, Json::text(origin)),
        (FORK_STATE, Json::String(payload)),
        (FORK_TURN, Json::Number(turn)),
    ])
}

pub fn fork_records(input: &Json) -> Result<[Record; 2], String> {
    let number = |key: &str| input.get(key).and_then(Json::as_u64).ok_or_else(|| format!("a fork's input lacks '{key}'"));
    let turn = number(FORK_TURN)?;
    let discarded = number(FORK_DISCARDED)?;
    let Some(Json::String(origin)) = input.get(FORK_ORIGIN) else {
        return Err(format!("a fork's input lacks '{FORK_ORIGIN}'"));
    };
    let Some(Json::String(payload)) = input.get(FORK_STATE) else {
        return Err(format!("a fork's input lacks '{FORK_STATE}'"));
    };
    Ok([
        Record::Host {
            key: FORKED_HOST_KEY.to_owned(),
            value: Json::object(vec![
                (FORK_DISCARDED, Json::Number(discarded)),
                (FORK_ORIGIN, Json::text(origin)),
                (FORK_TURN, Json::Number(turn)),
            ]),
        },
        Record::Checkpoint {
            revision: ENTRY_REVISION,
            state: program::HARNESS_NAME.to_owned(),
            payload: payload.clone(),
        },
    ])
}

pub fn fork_entry(start: Start, records: &[Record], at: Option<u64>, origin: &str) -> Result<[Record; 2], ForkRefused> {
    let session = fold_from(start, records).map_err(ForkRefused::Corrupt)?;
    let (turn, discarded) = fork_select(&session, at).map_err(ForkRefused::Missing)?;
    let payload = fork_payload(&session, turn, discarded, origin).map_err(|exit| match exit {
        Exit::Corrupt(error) => ForkRefused::Corrupt(error),
        Exit::Stop(stop) => ForkRefused::Corrupt(format!("the fork stopped: {stop:?}")),
    })?;
    fork_records(&fork_input(turn, discarded, origin, payload)).map_err(ForkRefused::Corrupt)
}
