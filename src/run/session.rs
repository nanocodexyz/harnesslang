use std::collections::BTreeMap;

use crate::effect::EffectClass;
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
}

impl TurnLog {
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub turns: BTreeMap<u64, TurnLog>,
    pub checkpoints: BTreeMap<u64, Json>,
}

impl Session {
    pub fn finished_turns(&self) -> u64 {
        self.turns.values().filter(|turn| turn.finished.is_some()).count() as u64
    }
}

fn running<'a>(session: &'a mut Session, turn: u64, what: &str) -> Result<&'a mut TurnLog, String> {
    let log = session.turns.get_mut(&turn).ok_or_else(|| format!("{what} for turn {turn}, which was not admitted"))?;
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
        Record::Admitted { turn, handler, arguments } => {
            if let Some((last, log)) = session.turns.last_key_value() {
                if *turn != last + 1 {
                    return Err(format!("turn {turn} is admitted after turn {last}"));
                }
                if log.finished.is_none() {
                    return Err(format!("turn {turn} is admitted before turn {last} finished"));
                }
            }
            session.turns.insert(*turn, TurnLog { handler: handler.clone(), arguments: arguments.clone(), sites: BTreeMap::new(), attempts: 0, finished: None });
        }
        Record::Call { turn, seq, tool, effect, input } => {
            let log = running(session, *turn, "a call")?;
            next_site(log, *turn, *seq, SiteLog::Open(Call { tool: tool.clone(), effect: *effect, input: input.clone() }))?;
        }
        Record::Result { turn, seq, output } => {
            let log = running(session, *turn, "a result")?;
            let site = log.sites.get_mut(seq).ok_or_else(|| format!("result for seq {seq} of turn {turn}, which has no call"))?;
            let SiteLog::Open(call) = site else {
                return Err(format!("result for seq {seq} of turn {turn}, which is not open"));
            };
            *site = SiteLog::Done(call.clone(), output.clone());
        }
        Record::Model { turn, seq, response } => {
            let log = running(session, *turn, "a model reply")?;
            next_site(log, *turn, *seq, SiteLog::Reply(response.clone()))?;
        }
        Record::Recovery { turn, attempt, unknown } => {
            let log = running(session, *turn, "a recovery")?;
            if *attempt <= log.attempts {
                return Err(format!("recovery attempt {attempt} of turn {turn} follows attempt {}", log.attempts));
            }
            log.attempts = *attempt;
            for seq in unknown {
                let site = log.sites.get_mut(seq).ok_or_else(|| format!("recovery of seq {seq} of turn {turn}, which has no call"))?;
                let SiteLog::Open(call) = site else {
                    return Err(format!("recovery of seq {seq} of turn {turn}, which is not open"));
                };
                if !call.effect.may_be_unknown() {
                    return Err(format!("recovery reports {} call seq {seq} of turn {turn} unknown", call.effect.name()));
                }
                *site = SiteLog::Unknown(call.clone());
            }
        }
        Record::Status { turn, last, outcome, result } => {
            let log = running(session, *turn, "a status")?;
            log.finished = Some(Finished { outcome: *outcome, result: result.clone(), last: *last });
        }
        Record::Checkpoint { revision, payload, .. } => {
            let finished = session.turns.get(revision).is_some_and(|log| log.finished.is_some());
            if !finished {
                return Err(format!("checkpoint at revision {revision}, which is not a finished turn"));
            }
            session.checkpoints.insert(*revision, payload.clone());
        }
    }
    Ok(())
}
