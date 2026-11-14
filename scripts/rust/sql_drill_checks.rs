use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use crate::harness::{self, EffectClass, Record, call_id};

const BUILTIN_EFFECTS: [&str; 2] = ["now", "random"];
const RUN_MARKER: &str = "run";
const ACQUIRED_MARKER: &str = "acquired";
const TAKEN_OVER_MARKER: &str = "taken_over";
const FINISHED_MARKER: &str = "finished";

pub fn append_synced(file: &mut std::fs::File, line: &str) {
    file.write_all(line.as_bytes())
        .expect("the drill file takes the line");
    file.sync_data().expect("the drill file syncs");
}

pub fn open_append(path: &str) -> std::fs::File {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|error| panic!("{path}: {error}"))
}

pub fn run_tag(owner: &str) -> String {
    assert!(!owner.is_empty(), "an owner has a name");
    assert!(!owner.contains(' '), "an owner is one word");
    format!("{owner}.{}", std::process::id())
}

pub fn run_line(tag: &str) -> String {
    format!("{RUN_MARKER} {tag}\n")
}

pub fn acquired_line(tag: &str, epoch: u64) -> String {
    format!("{ACQUIRED_MARKER} {tag} {epoch}\n")
}

pub fn taken_over_line(tag: &str) -> String {
    format!("{TAKEN_OVER_MARKER} {tag}\n")
}

pub fn finished_line(tag: &str) -> String {
    format!("{FINISHED_MARKER} {tag}\n")
}

pub fn dispatch_line(id: &str, tool: &str, effect: EffectClass, tag: &str) -> String {
    format!("{id} {tool} {} {tag}\n", effect.name())
}

pub struct Dispatch {
    pub id: String,
    pub tool: String,
    pub external: bool,
    pub tag: String,
}

impl Dispatch {
    pub fn nested(&self) -> bool {
        self.id.contains(harness::NESTED_ID_INFIX)
    }
}

pub enum Line {
    Run(String),
    Acquired(String, u64),
    TakenOver(String),
    Finished(String),
    Dispatch(Dispatch),
}

fn parse_line(line: &str) -> Line {
    let parts: Vec<&str> = line.split(' ').collect();
    match parts.as_slice() {
        [RUN_MARKER, tag] => Line::Run((*tag).to_owned()),
        [ACQUIRED_MARKER, tag, epoch] => Line::Acquired(
            (*tag).to_owned(),
            epoch
                .parse()
                .unwrap_or_else(|_| panic!("ledger line {line:?} has no epoch")),
        ),
        [TAKEN_OVER_MARKER, tag] => Line::TakenOver((*tag).to_owned()),
        [FINISHED_MARKER, tag] => Line::Finished((*tag).to_owned()),
        [id, tool, effect, tag] => Line::Dispatch(Dispatch {
            id: (*id).to_owned(),
            tool: (*tool).to_owned(),
            external: *effect == EffectClass::External.name(),
            tag: (*tag).to_owned(),
        }),
        _ => panic!("ledger line {line:?} is neither a marker nor 'id tool effect tag'"),
    }
}

pub fn read_ledger(ledger: &str) -> Vec<Line> {
    let text = std::fs::read_to_string(ledger).unwrap_or_default();
    let lines: Vec<Line> = text.lines().map(parse_line).collect();
    let mut started = BTreeSet::new();
    for line in &lines {
        match line {
            Line::Run(tag) => assert!(started.insert(tag.as_str()), "run {tag} starts once"),
            Line::Acquired(tag, _)
            | Line::TakenOver(tag)
            | Line::Finished(tag)
            | Line::Dispatch(Dispatch { tag, .. }) => {
                assert!(
                    started.contains(tag.as_str()),
                    "run {tag} starts before it writes"
                );
            }
        }
    }
    lines
}

pub fn dispatches(lines: &[Line]) -> impl Iterator<Item = &Dispatch> {
    lines.iter().filter_map(|line| match line {
        Line::Dispatch(dispatch) => Some(dispatch),
        _ => None,
    })
}

pub struct Logged {
    pub tool: String,
    pub done: bool,
}

pub struct Calls {
    pub external: BTreeMap<String, Logged>,
    pub other: BTreeMap<String, Logged>,
    pub unknown: BTreeSet<String>,
    pub statuses: BTreeMap<u64, Vec<(String, String)>>,
}

fn record_call(calls: &mut Calls, turn: u64, seq: u64, tool: &str, effect: EffectClass) {
    let map = if effect == EffectClass::External {
        &mut calls.external
    } else {
        &mut calls.other
    };
    let logged = Logged {
        tool: tool.to_owned(),
        done: false,
    };
    let id = call_id(turn, seq);
    assert!(
        map.insert(id.clone(), logged).is_none(),
        "seq_unique: call {id} recorded twice"
    );
}

pub fn calls(records: &[Record]) -> Calls {
    let mut calls = Calls {
        external: BTreeMap::new(),
        other: BTreeMap::new(),
        unknown: BTreeSet::new(),
        statuses: BTreeMap::new(),
    };
    for record in records {
        match record {
            Record::Call {
                turn,
                seq,
                tool,
                effect,
                ..
            } => record_call(&mut calls, *turn, *seq, tool, *effect),
            Record::Result { turn, seq, .. } => {
                let id = call_id(*turn, *seq);
                let logged = calls
                    .external
                    .get_mut(&id)
                    .or_else(|| calls.other.get_mut(&id))
                    .expect("a result follows its call");
                logged.done = true;
            }
            Record::Recovery { turn, unknown, .. } => {
                for seq in unknown {
                    calls.unknown.insert(call_id(*turn, *seq));
                }
            }
            Record::Status {
                turn,
                outcome,
                result,
                ..
            } => calls
                .statuses
                .entry(*turn)
                .or_default()
                .push((outcome.name().to_owned(), result.encode())),
            _ => {}
        }
    }
    calls
}

pub fn check_finished(session: &harness::Session, calls: &Calls, turns: u64) {
    for turn in 1..=turns {
        let finished = session
            .turns
            .get(&turn)
            .is_some_and(|log| log.finished.is_some());
        assert!(finished, "liveness: turn {turn} finished");
        let statuses = calls.statuses.get(&turn).map_or(0, Vec::len);
        assert_eq!(statuses, 1, "finish_once: turn {turn} has one status");
    }
}

pub fn check_external(calls: &Calls, lines: &[Line]) {
    let mut dispatched: BTreeMap<&str, u64> = BTreeMap::new();
    for dispatch in dispatches(lines).filter(|dispatch| dispatch.external && !dispatch.nested()) {
        *dispatched.entry(&dispatch.id).or_default() += 1;
        let logged = calls.external.get(&dispatch.id);
        assert!(
            logged.is_some(),
            "external {} ran before its call record was durable",
            dispatch.id
        );
        assert_eq!(
            logged.map(|logged| logged.tool.as_str()),
            Some(dispatch.tool.as_str()),
            "external {} ran as its recorded tool",
            dispatch.id
        );
    }
    for (id, count) in &dispatched {
        assert_eq!(*count, 1, "external_once: {id} dispatched {count} times");
    }
    for (id, logged) in &calls.external {
        let unknown = calls.unknown.contains(id);
        assert!(
            logged.done ^ unknown,
            "unknown_reported: external {id} has a result {} and is reported unknown {unknown}",
            logged.done
        );
        if logged.done {
            assert_eq!(
                dispatched.get(id.as_str()),
                Some(&1),
                "external {id} has a result without a dispatch"
            );
        }
    }
}

fn finished_tags(lines: &[Line]) -> BTreeSet<&str> {
    let finished: BTreeSet<&str> = lines
        .iter()
        .filter_map(|line| match line {
            Line::Finished(tag) => Some(tag.as_str()),
            _ => None,
        })
        .collect();
    assert!(!finished.is_empty(), "liveness: a run finished");
    finished
}

pub fn check_other(calls: &Calls, lines: &[Line]) {
    let finished = finished_tags(lines);
    for dispatch in dispatches(lines)
        .filter(|dispatch| {
            !dispatch.external && !dispatch.nested() && finished.contains(dispatch.tag.as_str())
        })
    {
        let logged = calls.other.get(&dispatch.id);
        assert!(
            logged.is_some(),
            "{} ran in the run that finished but its call record is not durable",
            dispatch.id
        );
        assert_eq!(
            logged.map(|logged| logged.tool.as_str()),
            Some(dispatch.tool.as_str()),
            "{} ran as its recorded tool",
            dispatch.id
        );
    }
    let ran: BTreeSet<(&str, &str)> = dispatches(lines)
        .filter(|dispatch| !dispatch.external)
        .map(|dispatch| (dispatch.id.as_str(), dispatch.tool.as_str()))
        .collect();
    for (id, logged) in &calls.other {
        assert!(logged.done, "idempotent_known: {id} ends without a result");
        if !BUILTIN_EFFECTS.contains(&logged.tool.as_str())
            && harness::program::code_of(&logged.tool).is_none()
        {
            assert!(
                ran.contains(&(id.as_str(), logged.tool.as_str())),
                "{id} has a result without a dispatch of {}",
                logged.tool
            );
        }
    }
}

fn lost_other(calls: &Calls, lines: &[Line]) -> usize {
    dispatches(lines)
        .filter(|dispatch| {
            !dispatch.external
                && !dispatch.nested()
                && !calls
                    .other
                    .get(&dispatch.id)
                    .is_some_and(|logged| logged.tool == dispatch.tool)
        })
        .count()
}

pub fn check_acks(calls: &Calls, acks: &str, turns: u64) -> usize {
    let acks_text = std::fs::read_to_string(acks).unwrap_or_default();
    let mut acked = BTreeSet::new();
    for line in acks_text.lines() {
        let mut parts = line.splitn(3, ' ');
        let (Some(turn), Some(outcome), Some(result)) = (parts.next(), parts.next(), parts.next())
        else {
            panic!("ack line {line:?} is not 'turn outcome result'");
        };
        let turn: u64 = turn.parse().expect("an ack names its turn");
        let status = &calls
            .statuses
            .get(&turn)
            .expect("an acknowledged turn has a status")[0];
        assert_eq!(
            (outcome, result),
            (status.0.as_str(), status.1.as_str()),
            "finish_once: turn {turn} acknowledged as its status"
        );
        acked.insert(turn);
    }
    assert_eq!(acked.len() as u64, turns, "every turn is acknowledged");
    acks_text.lines().count()
}

pub struct Fence {
    pub acquires: usize,
    pub takeovers: usize,
}

pub fn check_fence(lines: &[Line]) -> Fence {
    let mut holder: Option<(&str, u64)> = None;
    let mut fence = Fence {
        acquires: 0,
        takeovers: 0,
    };
    for line in lines {
        match line {
            Line::Acquired(tag, epoch) => {
                if let Some((previous, previous_epoch)) = holder {
                    assert!(
                        *epoch > previous_epoch,
                        "fence: run {tag} acquired epoch {epoch} after run {previous} held epoch {previous_epoch}"
                    );
                    if previous != tag {
                        fence.takeovers += 1;
                    }
                }
                holder = Some((tag, *epoch));
                fence.acquires += 1;
            }
            Line::Dispatch(dispatch) if dispatch.external => {
                if let Some((owner, epoch)) = holder {
                    assert_eq!(
                        dispatch.tag, owner,
                        "fence: external {} dispatched by run {} after run {owner} acquired epoch {epoch}",
                        dispatch.id, dispatch.tag
                    );
                }
            }
            _ => {}
        }
    }
    fence
}

pub fn check_taken_over(lines: &[Line], expected: usize) {
    let mut last_acquired: BTreeMap<&str, usize> = BTreeMap::new();
    let mut taken = 0;
    for (index, line) in lines.iter().enumerate() {
        match line {
            Line::Acquired(tag, _) => {
                last_acquired.insert(tag, index);
            }
            Line::TakenOver(tag) => {
                let own = last_acquired.get(tag.as_str()).copied();
                assert!(own.is_some(), "taken_over: run {tag} never acquired");
                let later_owner = last_acquired
                    .iter()
                    .any(|(other, at)| *other != tag && Some(*at) > own);
                assert!(
                    later_owner,
                    "taken_over: only a stale owner writes the marker, but no run acquired after {tag}"
                );
                taken += 1;
            }
            _ => {}
        }
    }
    assert_eq!(
        taken, expected,
        "taken_over: {taken} markers where the drill expects {expected}"
    );
}

pub struct Verified {
    pub runs: usize,
    pub external: usize,
    pub unknown: usize,
    pub other: usize,
    pub dispatches: usize,
    pub other_lost: usize,
    pub acks: usize,
    pub fence: Fence,
}

pub fn check_all(
    records: &[Record],
    ledger: &str,
    acks: &str,
    turns: u64,
    taken_over: usize,
) -> Verified {
    let session =
        harness::fold(records).unwrap_or_else(|error| panic!("the journal folds: {error}"));
    let calls = calls(records);
    let lines = read_ledger(ledger);
    let fence = check_fence(&lines);
    check_external(&calls, &lines);
    check_taken_over(&lines, taken_over);
    check_other(&calls, &lines);
    check_finished(&session, &calls, turns);
    let acks = check_acks(&calls, acks, turns);
    Verified {
        runs: lines
            .iter()
            .filter(|line| matches!(line, Line::Run(_)))
            .count(),
        external: calls.external.len(),
        unknown: calls.unknown.len(),
        other: calls.other.len(),
        dispatches: dispatches(&lines).count(),
        other_lost: lost_other(&calls, &lines),
        acks,
        fence,
    }
}
