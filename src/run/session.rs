use std::collections::BTreeMap;

use crate::effect::EffectClass;
use crate::run::cell_journal::{CellLog, CellUpdate};
use crate::run::journal::{Outcome, Output, Record};
use crate::run::json::Json;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub tool: String,
    pub effect: EffectClass,
    pub input: Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SiteLog {
    Open(Call),
    Done(Call, Output),
    Unknown(Call),
    Reply(Json),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub outcome: Outcome,
    pub result: Json,
    pub last: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnLog {
    pub handler: String,
    pub arguments: Json,
    pub sites: BTreeMap<u64, SiteLog>,
    pub attempts: u64,
    pub finished: Option<Finished>,
    pub nested: BTreeMap<(u64, u64), SiteLog>,
    pub cells: BTreeMap<u64, CellLog>,
}

impl TurnLog {
    pub fn new(handler: String, arguments: Json) -> Self {
        Self {
            handler,
            arguments,
            sites: BTreeMap::new(),
            attempts: 0,
            finished: None,
            nested: BTreeMap::new(),
            cells: BTreeMap::new(),
        }
    }

    pub fn open_nested(&self, class: impl Fn(EffectClass) -> bool) -> Vec<(u64, u64)> {
        self.nested
            .iter()
            .filter_map(|(id, site)| match site {
                SiteLog::Open(call) if class(call.effect) => Some(*id),
                _ => None,
            })
            .collect()
    }

    pub fn open_seqs(&self, class: impl Fn(EffectClass) -> bool) -> Vec<u64> {
        self.sites
            .iter()
            .filter_map(|(seq, site)| match site {
                SiteLog::Open(call) if class(call.effect) => Some(*seq),
                _ => None,
            })
            .collect()
    }
}

pub const ENTRY_REVISION: u64 = 0;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub turns: BTreeMap<u64, TurnLog>,
    pub checkpoints: BTreeMap<u64, String>,
    pub cell_origins: BTreeMap<u64, (u64, u64)>,
}

impl Session {
    pub fn next_cell(&self) -> u64 {
        self.cell_origins
            .last_key_value()
            .map_or(1, |(cell, _)| cell + 1)
    }

    pub fn finished_turns(&self) -> u64 {
        self.turns
            .values()
            .filter(|turn| turn.finished.is_some())
            .count() as u64
    }
}

fn call_of(tool: &str, effect: EffectClass, input: &Json) -> Call {
    Call {
        tool: tool.to_owned(),
        effect,
        input: input.clone(),
    }
}

fn nested_call(log: &mut TurnLog, turn: u64, id: (u64, u64), call: Call) -> Result<(), String> {
    let (seq, nested) = id;
    if !matches!(log.sites.get(&seq), Some(SiteLog::Open(_))) {
        return Err(format!(
            "nested call {seq}/{nested} of turn {turn} comes from a site that is not running"
        ));
    }
    if let Some(((last_seq, last), _)) = log.nested.range((seq, 0)..=(seq, u64::MAX)).next_back()
        && (*last_seq, *last) >= id
    {
        return Err(format!(
            "turn {turn} records nested call {seq}/{nested} after {seq}/{last}"
        ));
    }
    log.nested.insert(id, SiteLog::Open(call));
    Ok(())
}

fn recover_nested(log: &mut TurnLog, turn: u64, id: (u64, u64)) -> Result<(), String> {
    let (seq, nested) = id;
    let site = log.nested.get_mut(&id).ok_or_else(|| {
        format!("recovery of nested call {seq}/{nested} of turn {turn}, which has no call")
    })?;
    let SiteLog::Open(call) = site else {
        return Err(format!(
            "recovery of nested call {seq}/{nested} of turn {turn}, which is not open"
        ));
    };
    if !call.effect.may_be_unknown() {
        return Err(format!(
            "recovery reports {} nested call {seq}/{nested} of turn {turn} unknown",
            call.effect.name()
        ));
    }
    *site = SiteLog::Unknown(call.clone());
    Ok(())
}

fn apply_cell(
    session: &mut Session,
    cell: u64,
    fresh: bool,
    update: &CellUpdate,
) -> Result<(), String> {
    if !fresh {
        let (turn, seq) = *session
            .cell_origins
            .get(&cell)
            .ok_or_else(|| format!("cell {cell} has an update before its header"))?;
        let log = running(session, turn, "a cell update")?;
        let folded = log
            .cells
            .get_mut(&seq)
            .ok_or_else(|| format!("cell {cell} has lost its origin"))?;
        return folded.apply(update);
    }
    if cell < session.next_cell() {
        return Err(format!(
            "cell {cell} starts after cell {}",
            session.next_cell() - 1
        ));
    }
    let header = update
        .header
        .as_ref()
        .ok_or_else(|| format!("fresh cell {cell} lacks its header"))?;
    let (turn, seq, nested) = crate::run::journal::parse_any_call_id(&header.origin_call_id)?;
    if nested.is_some() {
        return Err(format!("cell {cell} starts from a nested call"));
    }
    let log = running(session, turn, "a cell")?;
    if !matches!(log.sites.get(&seq), Some(SiteLog::Open(_))) {
        return Err(format!(
            "cell {cell} starts from {turn}/{seq}, which is not a running site"
        ));
    }
    if log.cells.contains_key(&seq) {
        return Err(format!(
            "cell {cell} starts from {turn}/{seq}, which already has a cell"
        ));
    }
    log.cells.insert(seq, CellLog::start(update)?);
    session.cell_origins.insert(cell, (turn, seq));
    Ok(())
}

fn running<'a>(session: &'a mut Session, turn: u64, what: &str) -> Result<&'a mut TurnLog, String> {
    let log = session
        .turns
        .get_mut(&turn)
        .ok_or_else(|| format!("{what} for turn {turn}, which was not admitted"))?;
    if log.finished.is_some() {
        return Err(format!("{what} for turn {turn}, which already finished"));
    }
    Ok(log)
}

fn next_site(log: &mut TurnLog, turn: u64, seq: u64, site: SiteLog) -> Result<(), String> {
    if let Some((last, _)) = log.sites.last_key_value()
        && seq <= *last
    {
        return Err(format!("turn {turn} records seq {seq} after seq {last}"));
    }
    log.sites.insert(seq, site);
    Ok(())
}

pub fn fold(records: &[Record]) -> Result<Session, String> {
    let mut session = Session::default();
    for record in records {
        apply(&mut session, record)?;
    }
    Ok(session)
}

pub fn apply(session: &mut Session, record: &Record) -> Result<(), String> {
    match record {
        Record::Admitted {
            turn,
            handler,
            arguments,
        } => {
            if let Some((last, log)) = session.turns.last_key_value() {
                if *turn != last + 1 {
                    return Err(format!("turn {turn} is admitted after turn {last}"));
                }
                if log.finished.is_none() {
                    return Err(format!(
                        "turn {turn} is admitted before turn {last} finished"
                    ));
                }
            }
            session
                .turns
                .insert(*turn, TurnLog::new(handler.clone(), arguments.clone()));
        }
        Record::Call {
            turn,
            seq,
            tool,
            effect,
            input,
        } => {
            let log = running(session, *turn, "a call")?;
            next_site(
                log,
                *turn,
                *seq,
                SiteLog::Open(Call {
                    tool: tool.clone(),
                    effect: *effect,
                    input: input.clone(),
                }),
            )?;
        }
        Record::Result { turn, seq, output } => {
            let log = running(session, *turn, "a result")?;
            let site = log
                .sites
                .get_mut(seq)
                .ok_or_else(|| format!("result for seq {seq} of turn {turn}, which has no call"))?;
            let SiteLog::Open(call) = site else {
                return Err(format!(
                    "result for seq {seq} of turn {turn}, which is not open"
                ));
            };
            *site = SiteLog::Done(call.clone(), output.clone());
        }
        Record::Model {
            turn,
            seq,
            response,
        } => {
            let log = running(session, *turn, "a model reply")?;
            next_site(log, *turn, *seq, SiteLog::Reply(response.clone()))?;
        }
        Record::NestedCall {
            turn,
            seq,
            nested,
            tool,
            effect,
            input,
        } => {
            let log = running(session, *turn, "a nested call")?;
            nested_call(log, *turn, (*seq, *nested), call_of(tool, *effect, input))?;
        }
        Record::NestedResult {
            turn,
            seq,
            nested,
            output,
        } => {
            let log = running(session, *turn, "a nested result")?;
            let site = log.nested.get_mut(&(*seq, *nested)).ok_or_else(|| {
                format!("result for nested call {seq}/{nested} of turn {turn}, which has no call")
            })?;
            let SiteLog::Open(call) = site else {
                return Err(format!(
                    "result for nested call {seq}/{nested} of turn {turn}, which is not open"
                ));
            };
            *site = SiteLog::Done(call.clone(), output.clone());
        }
        Record::Cell {
            cell,
            fresh,
            update,
            ..
        } => apply_cell(session, *cell, *fresh, update)?,
        Record::Recovery {
            turn,
            attempt,
            unknown,
            nested,
        } => {
            let log = running(session, *turn, "a recovery")?;
            for id in nested {
                recover_nested(log, *turn, *id)?;
            }
            if *attempt <= log.attempts {
                return Err(format!(
                    "recovery attempt {attempt} of turn {turn} follows attempt {}",
                    log.attempts
                ));
            }
            log.attempts = *attempt;
            for seq in unknown {
                let site = log.sites.get_mut(seq).ok_or_else(|| {
                    format!("recovery of seq {seq} of turn {turn}, which has no call")
                })?;
                let SiteLog::Open(call) = site else {
                    return Err(format!(
                        "recovery of seq {seq} of turn {turn}, which is not open"
                    ));
                };
                if !call.effect.may_be_unknown() {
                    return Err(format!(
                        "recovery reports {} call seq {seq} of turn {turn} unknown",
                        call.effect.name()
                    ));
                }
                *site = SiteLog::Unknown(call.clone());
            }
        }
        Record::Status {
            turn,
            last,
            outcome,
            result,
        } => {
            let log = running(session, *turn, "a status")?;
            log.finished = Some(Finished {
                outcome: *outcome,
                result: result.clone(),
                last: *last,
            });
        }
        Record::Checkpoint {
            revision, payload, ..
        } => {
            let finished = session
                .turns
                .get(revision)
                .is_some_and(|log| log.finished.is_some());
            let entry = *revision == ENTRY_REVISION && session.turns.is_empty();
            if !finished && !entry {
                return Err(format!(
                    "checkpoint at revision {revision}, which is not a finished turn"
                ));
            }
            session.checkpoints.insert(*revision, payload.clone());
        }
    }
    Ok(())
}
