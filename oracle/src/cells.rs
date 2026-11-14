use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use nanocode_code_mode::code_mode::{
    CELL_JOURNAL_VERSION, CellJournal, CellJournalHeader, CellJournalUpdate, JournaledCall,
    JournaledCallState,
};
use nanocode_code_mode_engine::JournalEntry;
use serde_json::{Value, json};

pub const UNKNOWN_OUTCOME_TEXT: &str = "This nested call was running when its cell moved to another host; its outcome is unknown. Do not assume it did not run or automatically repeat it.";
pub const CALL_INTERRUPTED_CODE: &str = "CODE_MODE_CALL_INTERRUPTED";
pub const UNKNOWN_OUTCOME_NAME: &str = "unknown";

pub fn unknown_outcome() -> String {
    json!({
        "error": UNKNOWN_OUTCOME_TEXT,
        "code": CALL_INTERRUPTED_CODE,
        "outcome": UNKNOWN_OUTCOME_NAME,
    })
    .to_string()
}

pub fn same_step(left: &JournalEntry, right: &JournalEntry) -> bool {
    match (left, right) {
        (
            JournalEntry::Timeout {
                id: left_id,
                delay_ms: left_delay,
                ..
            },
            JournalEntry::Timeout {
                id: right_id,
                delay_ms: right_delay,
                ..
            },
        ) => left_id == right_id && left_delay == right_delay,
        (JournalEntry::Clock { .. }, JournalEntry::Clock { .. })
        | (JournalEntry::Monotonic { .. }, JournalEntry::Monotonic { .. })
        | (JournalEntry::Random { .. }, JournalEntry::Random { .. }) => true,
        (JournalEntry::Bytes { data: left }, JournalEntry::Bytes { data: right }) => {
            left.len() == right.len()
        }
        (left, right) => left == right,
    }
}

pub fn steps_agree(left: &JournalEntry, right: &JournalEntry) -> bool {
    if left.is_input() || right.is_input() {
        left == right
    } else {
        same_step(left, right)
    }
}

pub fn first_difference(left: &[JournalEntry], right: &[JournalEntry]) -> Option<String> {
    for (index, (one, other)) in left.iter().zip(right).enumerate() {
        if !steps_agree(one, other) {
            return Some(format!(
                "entry {index}: {} against {}",
                one.describe(),
                other.describe()
            ));
        }
    }
    if left.len() != right.len() {
        return Some(format!("{} entries against {}", left.len(), right.len()));
    }
    None
}

pub struct Header {
    pub session: String,
    pub cell: u64,
    pub origin: String,
    pub script: String,
    pub tools: Vec<String>,
    pub yield_ms: u64,
}

fn header(meta: &Header) -> CellJournalHeader {
    CellJournalHeader {
        version: CELL_JOURNAL_VERSION,
        origin_call_id: meta.origin.clone(),
        script: meta.script.clone(),
        tools: meta
            .tools
            .iter()
            .map(|name| Value::String(name.clone()))
            .collect::<Vec<_>>()
            .into(),
        stored: BTreeMap::new(),
        prelude: false,
        yield_time_ms: meta.yield_ms,
        max_output_tokens: None,
    }
}

fn update(meta: &Header, seq: u64, entries_from: usize) -> CellJournalUpdate {
    CellJournalUpdate {
        session_id: meta.session.clone(),
        cell_id: meta.cell,
        seq,
        header: None,
        entries_from,
        entries: Vec::new(),
        calls: Vec::new(),
        observer_call_id: None,
        observer_yield_ms: None,
        observer_since_ms: None,
        content_returned: 0,
        finished: false,
    }
}

fn call_of(
    meta: &Header,
    entry: &JournalEntry,
    calls: &mut BTreeMap<u64, JournaledCall>,
) -> Option<Arc<JournaledCall>> {
    match entry {
        JournalEntry::Call {
            id,
            name,
            input,
            host,
        } => {
            let call = JournaledCall {
                id: *id,
                call_id: format!("{}/code-{id}", meta.origin),
                name: name.clone(),
                input: input.clone(),
                host: *host,
                state: JournaledCallState::Running,
            };
            calls.insert(*id, call.clone());
            Some(Arc::new(call))
        }
        JournalEntry::Result {
            id,
            encoded,
            success,
        } => {
            let call = calls.get_mut(id)?;
            call.state = JournaledCallState::Completed {
                encoded: encoded.clone(),
                success: *success,
            };
            Some(Arc::new(call.clone()))
        }
        _ => None,
    }
}

pub fn model_updates(
    meta: &Header,
    entries: &[JournalEntry],
    finished: bool,
) -> Vec<CellJournalUpdate> {
    let mut updates = Vec::with_capacity(entries.len() + 2);
    let mut first = update(meta, 0, 0);
    first.header = Some(header(meta));
    updates.push(first);
    let mut calls = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let mut next = update(meta, updates.len() as u64, index);
        next.entries.push(entry.clone());
        next.calls.extend(call_of(meta, entry, &mut calls));
        updates.push(next);
    }
    if finished {
        let mut last = update(meta, updates.len() as u64, entries.len());
        last.finished = true;
        updates.push(last);
    }
    updates
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoldState {
    pub entries: usize,
    pub calls: BTreeMap<u64, String>,
    pub finished: bool,
    pub unknown: BTreeSet<u64>,
}

pub fn state_name(state: &JournaledCallState) -> String {
    match state {
        JournaledCallState::Queued => "queued".to_owned(),
        JournaledCallState::Running => "running".to_owned(),
        JournaledCallState::Completed { .. } => "completed".to_owned(),
    }
}

pub fn fold(updates: &[CellJournalUpdate]) -> Option<(CellJournal, FoldState)> {
    let journal = CellJournal::fold(updates.iter().cloned())?;
    let state = FoldState {
        entries: journal.entries.len(),
        calls: journal
            .calls
            .iter()
            .map(|call| (call.id, state_name(&call.state)))
            .collect(),
        finished: journal.finished,
        unknown: journal.unknown_calls().map(|call| call.id).collect(),
    };
    Some((journal, state))
}

pub fn expected_state(entries: &[JournalEntry], finished: bool) -> FoldState {
    let mut running = BTreeSet::new();
    for entry in entries {
        match entry {
            JournalEntry::Call { id, .. } => {
                running.insert(*id);
            }
            JournalEntry::Result { id, .. } => {
                running.remove(id);
            }
            _ => {}
        }
    }
    FoldState {
        entries: entries.len(),
        calls: running
            .iter()
            .map(|id| (*id, "running".to_owned()))
            .collect(),
        finished,
        unknown: running,
    }
}

pub fn check_prefixes(
    updates: &[CellJournalUpdate],
    entries: &[JournalEntry],
    finished: bool,
) -> Result<Vec<FoldState>, String> {
    let mut states = Vec::with_capacity(updates.len());
    let mut consumed = 0;
    for count in 1..=updates.len() {
        let prefix = &updates[..count];
        let (journal, state) =
            fold(prefix).ok_or_else(|| format!("prefix of {count} updates folds to nothing"))?;
        consumed += prefix[count - 1].entries.len();
        let last = prefix[count - 1].finished && finished;
        let wanted = expected_state(&entries[..consumed.min(entries.len())], last);
        if state != wanted {
            return Err(format!(
                "prefix of {count} updates: nanocode {state:?}, model {wanted:?}"
            ));
        }
        if journal.entries.as_slice() != &entries[..state.entries] {
            return Err(format!("prefix of {count} updates: entries differ"));
        }
        states.push(state);
    }
    Ok(states)
}

pub fn check_gap(updates: &[CellJournalUpdate]) -> Result<(), String> {
    let Some(gap) = (2..updates.len()).find(|index| !updates[*index].entries.is_empty()) else {
        return Ok(());
    };
    let mut holed: Vec<CellJournalUpdate> = updates.to_vec();
    let removed = holed.remove(gap - 1);
    let whole = fold(updates).map(|(_, state)| state.entries);
    let after = fold(&holed).map(|(_, state)| state.entries);
    match (whole, after) {
        (Some(whole), Some(after)) if after < whole && after <= removed.entries_from => Ok(()),
        other => Err(format!(
            "removing update {} did not stop the fold: {other:?}",
            removed.seq
        )),
    }
}
