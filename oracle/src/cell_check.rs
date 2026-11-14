use std::collections::{BTreeMap, BTreeSet};

use nanocode_code_mode::code_mode::{CellJournal, CellJournalUpdate, JournaledCallState};
use nanocode_code_mode_engine::JournalEntry;
use nanocode_turnlog::Fold;
use serde_json::Value;
use serde_json::value::RawValue;

use crate::cells;
use crate::djs::{self, Script, Terminal};
use crate::golden::Follow;
use crate::summary::CellSummary;

const NESTED_ID_INFIX: &str = "/code-";
const DIFFERENTIAL_SEED: u64 = 0;
const DIFFERENTIAL_WALL_MS: u64 = 0;

pub struct CellView {
    pub journal: CellJournal,
    pub updates: usize,
    pub consumed: usize,
}

fn decode(raw: &[Box<RawValue>]) -> Result<Vec<CellJournalUpdate>, String> {
    raw.iter()
        .enumerate()
        .map(|(index, update)| {
            serde_json::from_str(update.get()).map_err(|error| {
                format!("nanocode cannot decode update {index} as CellJournalUpdate: {error}")
            })
        })
        .collect()
}

fn consumed(updates: &[CellJournalUpdate]) -> usize {
    let mut sorted: Vec<&CellJournalUpdate> = updates.iter().collect();
    sorted.sort_by_key(|update| update.seq);
    let Some(start) = sorted.iter().position(|update| update.header.is_some()) else {
        return 0;
    };
    let mut seq = sorted[start].seq;
    let mut entries = 0;
    let mut count = 0;
    for update in &sorted[start..] {
        if update.seq != seq || update.entries_from > entries {
            break;
        }
        seq += 1;
        entries = entries.max(update.entries_from + update.entries.len());
        count += 1;
    }
    count
}

pub fn view(fold: &Fold, session: &str, cell: u64) -> Result<CellView, String> {
    let raw = fold
        .cells()
        .find(|(name, id, _)| *name == session && *id == cell)
        .map(|(_, _, updates)| updates)
        .ok_or_else(|| format!("nanocode's fold holds no updates of cell {cell}"))?;
    let updates = decode(raw)?;
    let count = updates.len();
    let consumed = consumed(&updates);
    let journal = CellJournal::fold(updates)
        .ok_or_else(|| format!("CellJournal::fold of cell {cell} finds no header"))?;
    Ok(CellView {
        journal,
        updates: count,
        consumed,
    })
}

pub fn accepts(fold: &Fold, session: &str, cell: u64) -> Result<(), String> {
    let view = view(fold, session, cell)?;
    if view.consumed != view.updates {
        return Err(format!(
            "CellJournal::fold of cell {cell} stops after {} of its {} updates",
            view.consumed, view.updates
        ));
    }
    Ok(())
}

fn state_names(journal: &CellJournal) -> BTreeMap<u64, String> {
    journal
        .calls
        .iter()
        .map(|call| (call.id, cells::state_name(&call.state)))
        .collect()
}

pub fn compare(view: &CellView, ours: &CellSummary) -> Vec<String> {
    let journal = &view.journal;
    let mut wrong = Vec::new();
    let entries: Result<Vec<JournalEntry>, _> = ours
        .entries
        .iter()
        .map(|entry| serde_json::from_value::<JournalEntry>(entry.clone()))
        .collect();
    match entries {
        Ok(entries) if entries == journal.entries => {}
        Ok(entries) => wrong.push(format!(
            "entries: nanocode {} entries, interpreter {}{}",
            journal.entries.len(),
            entries.len(),
            cells::first_difference(&journal.entries, &entries)
                .map_or(String::new(), |difference| format!(" ({difference})"))
        )),
        Err(error) => wrong.push(format!(
            "the interpreter's entries do not decode as JournalEntry: {error}"
        )),
    }
    let calls = state_names(journal);
    if calls != ours.calls {
        wrong.push(format!(
            "calls: nanocode {calls:?}, interpreter {:?}",
            ours.calls
        ));
    }
    if journal.finished != ours.finished {
        wrong.push(format!(
            "finished: nanocode {}, interpreter {}",
            journal.finished, ours.finished
        ));
    }
    let unknown: BTreeSet<u64> = journal.unknown_calls().map(|call| call.id).collect();
    let reported: BTreeSet<u64> = ours.unknown.iter().copied().collect();
    if unknown != reported {
        wrong.push(format!(
            "unknown calls: nanocode {unknown:?}, interpreter {reported:?}"
        ));
    }
    if journal.origin_call_id != ours.origin || journal.cell_id != ours.cell {
        wrong.push(format!(
            "origin: nanocode cell {} of {}, interpreter cell {} of {}",
            journal.cell_id, journal.origin_call_id, ours.cell, ours.origin
        ));
    }
    if view.consumed as u64 != ours.seq {
        wrong.push(format!(
            "updates: nanocode's fold consumes {}, the interpreter's next seq is {}",
            view.consumed, ours.seq
        ));
    }
    wrong
}

pub fn nested(call_id: &str) -> Option<(&str, u64)> {
    let (origin, id) = call_id.split_once(NESTED_ID_INFIX)?;
    Some((origin, id.parse().ok()?))
}

pub fn by_origin(fold: &Fold, origin: &str) -> Option<CellView> {
    fold.cells().find_map(|(session, cell, raw)| {
        let first = decode(raw.get(..1)?).ok()?.pop()?;
        if first.header?.origin_call_id != origin {
            return None;
        }
        view(fold, session, cell).ok()
    })
}

pub fn durable_before_dispatch(
    fold: &Fold,
    call_id: &str,
    tool: &str,
    input: &Value,
) -> Option<String> {
    let (origin, id) = nested(call_id)?;
    let Some(view) = by_origin(fold, origin) else {
        return Some(format!(
            "nested call {call_id} is recorded before its cell's header"
        ));
    };
    let entry = view.journal.entries.iter().find_map(|entry| match entry {
        JournalEntry::Call {
            id: found,
            name,
            input,
            ..
        } if *found == id => Some((name, input)),
        _ => None,
    });
    match entry {
        Some((name, found)) if name == tool && found == input => None,
        Some((name, found)) => Some(format!(
            "nested call {call_id} records {tool} {input}; its cell's Call entry is {name} {found}"
        )),
        None => Some(format!(
            "nested call {call_id} is recorded before its cell's Call entry"
        )),
    }
}

pub fn unknown_delivered(journal: &CellJournal, ids: &BTreeSet<u64>) -> Vec<String> {
    let outcome: Value = serde_json::from_str(&cells::unknown_outcome())
        .expect("the oracle's unknown outcome is JSON");
    ids.iter()
        .filter(|id| {
            !journal.entries.iter().any(|entry| match entry {
                JournalEntry::Result {
                    id: found,
                    encoded,
                    success: false,
                } => {
                    found == *id
                        && serde_json::from_str::<Value>(encoded).ok().as_ref() == Some(&outcome)
                }
                _ => false,
            })
        })
        .map(|id| {
            format!(
                "cell {} finishes without delivering nanocode's unknown outcome to nested call {id}, which recovery listed",
                journal.cell_id
            )
        })
        .collect()
}

pub fn open_running(journal: &CellJournal) -> BTreeSet<u64> {
    journal
        .calls
        .iter()
        .filter(|call| matches!(call.state, JournaledCallState::Running))
        .map(|call| call.id)
        .collect()
}

pub enum SiteOutput {
    Ok(String),
    Failed,
}

fn tool_names(journal: &CellJournal) -> Result<Vec<String>, String> {
    journal
        .tools
        .iter()
        .map(|tool| {
            tool.as_str().map(str::to_owned).ok_or_else(|| {
                format!(
                    "cell {} header lists a tool that is not a name: {tool}",
                    journal.cell_id
                )
            })
        })
        .collect()
}

pub fn differential(journal: &CellJournal, output: &SiteOutput) -> Result<(), String> {
    let script = Script {
        id: journal.cell_id,
        script: journal.script.clone(),
        tools: tool_names(journal)?,
        seed: DIFFERENTIAL_SEED,
        wall_ms: DIFFERENTIAL_WALL_MS,
    };
    let entries = &journal.entries;
    let mut follow = Follow { expected: entries };
    let run = djs::run(&script, Some(entries.clone()), &mut follow)?;
    if let Some(difference) = cells::first_difference(&run.entries, entries) {
        return Err(format!(
            "djs replaying cell {} writes a different journal: {difference}",
            journal.cell_id
        ));
    }
    match (&run.terminal, output) {
        (Terminal::Done, SiteOutput::Ok(text)) if *text == run.text => Ok(()),
        (Terminal::Threw(_), SiteOutput::Failed) => Ok(()),
        (terminal, SiteOutput::Ok(text)) => Err(format!(
            "djs replaying cell {} ends {terminal:?} with text {:?}; the site's result is ok({text:?})",
            journal.cell_id, run.text
        )),
        (terminal, SiteOutput::Failed) => Err(format!(
            "djs replaying cell {} ends {terminal:?} with text {:?}; the site's result is failed",
            journal.cell_id, run.text
        )),
    }
}
