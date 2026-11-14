#[path = "drill_cells.rs"]
mod drill_cells;
#[path = "harness.rs"]
mod harness;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use harness::program::{self, Arguments};
use harness::{
    CellEngine, Disk, EffectClass, FileDisk, Finish, Host, Json, JsonlJournal, ModelRequest, Output,
    Ran, Record, Returns, RunError, Start, ToolRequest, TurnRequest, call_id, nested_call_id,
};

const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
const SPLITMIX_MIX_FIRST: u64 = 0xbf58_476d_1ce4_e5b9;
const SPLITMIX_MIX_SECOND: u64 = 0x94d0_49bb_1331_11eb;
const PPM: u64 = 1_000_000;
const ANSWER_PPM: u64 = 350_000;
const UNDECLARED_TOOL_PPM: u64 = 250_000;
const TOOL_FAILURE_PPM: u64 = 150_000;
const UNDECLARED_TOOLS: [&str; 2] = ["web_fetch", "spawn_agent"];
const BUILTIN_EFFECTS: [&str; 2] = ["now", "random"];
const RUN_MARKER: &str = "run";

struct Rng(u64);

impl Rng {
    fn draw(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(SPLITMIX_GAMMA);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(SPLITMIX_MIX_FIRST);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(SPLITMIX_MIX_SECOND);
        mixed ^ (mixed >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "a draw is below a positive bound");
        ((u128::from(self.draw()) * u128::from(bound)) >> 64) as u64
    }

    fn chance(&mut self, ppm: u64) -> bool {
        self.below(PPM) < ppm
    }
}

fn append_synced(file: &mut std::fs::File, line: &str) {
    file.write_all(line.as_bytes())
        .expect("the drill file takes the line");
    file.sync_data().expect("the drill file syncs");
}

fn open_append(path: &str) -> std::fs::File {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|error| panic!("{path}: {error}"))
}

struct Drill {
    ledger: std::fs::File,
    acks: std::fs::File,
    rng: Rng,
    clock: u64,
    engine: drill_cells::ScriptEngine,
}

impl Host for Drill {
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        let id = match request.nested {
            Some(nested) => nested_call_id(&call_id(request.turn, request.seq), nested.id),
            None => call_id(request.turn, request.seq),
        };
        if let Some(nested) = request.nested
            && request.effect == EffectClass::External
        {
            drill_cells::check_c1(&self.engine.journal, request.turn, request.seq, nested.id, request.tool);
        }
        append_synced(
            &mut self.ledger,
            &format!("{id} {} {}\n", request.tool, request.effect.name()),
        );
        if self.rng.chance(TOOL_FAILURE_PPM) {
            return Output::Failed(format!("{} failed at {id}", request.tool));
        }
        Output::Ok(match request.returns {
            Returns::U64 => Json::Number(self.rng.draw() >> 32),
            Returns::Bool => Json::Bool(self.rng.draw() & 1 == 1),
            Returns::Text => Json::String(format!("{}-{id}", request.tool)),
        })
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        if self.rng.chance(ANSWER_PPM) {
            return Json::object(vec![(
                "answer",
                Json::String(format!("done-{}-{}", request.turn, request.seq)),
            )]);
        }
        let declared = ["read", "edit", "bash"];
        let count = 1 + self.rng.below(request.calls_max);
        let mut calls = Vec::new();
        for index in 0..count {
            if !program::CODES.is_empty() && self.rng.chance(drill_cells::CELL_REPLY_PPM) {
                let code = self.rng.below(program::CODES.len() as u64) as usize;
                let script = drill_cells::script(&mut self.rng, code, request.turn, request.seq);
                calls.push(Json::object(vec![
                    ("id", Json::String(format!("c{}-{}-{index}", request.turn, request.seq))),
                    ("tool", Json::text(program::CODES[code].name)),
                    ("input", Json::String(script)),
                ]));
                continue;
            }
            let tool = if self.rng.chance(UNDECLARED_TOOL_PPM) {
                UNDECLARED_TOOLS[self.rng.below(UNDECLARED_TOOLS.len() as u64) as usize]
            } else {
                declared[self.rng.below(declared.len() as u64) as usize]
            };
            calls.push(Json::object(vec![
                (
                    "id",
                    Json::String(format!("c{}-{}-{index}", request.turn, request.seq)),
                ),
                ("tool", Json::text(tool)),
                ("input", Json::String(format!("input-{index}"))),
            ]));
        }
        Json::object(vec![("calls", Json::Array(calls))])
    }

    fn now(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn random(&mut self) -> u64 {
        self.rng.draw()
    }

    fn acknowledge(&mut self, finish: &Finish) {
        append_synced(
            &mut self.acks,
            &format!(
                "{} {} {}\n",
                finish.turn,
                finish.outcome.name(),
                finish.result.encode()
            ),
        );
    }

    fn cells(&mut self) -> Option<&mut dyn CellEngine> {
        Some(&mut self.engine)
    }
}

fn request(seed: u64, turn: u64) -> TurnRequest {
    TurnRequest {
        turn,
        arguments: Arguments::Prompt(format!("drill-{seed}-{turn}")),
    }
}

fn run(journal: &str, ledger: &str, acks: &str, seed: u64, turns: u64) {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_nanos() as u64;
    let mut drill = Drill {
        ledger: open_append(ledger),
        acks: open_append(acks),
        rng: Rng(seed ^ started),
        clock: 0,
        engine: drill_cells::ScriptEngine::new(journal),
    };
    append_synced(&mut drill.ledger, &format!("{RUN_MARKER}\n"));
    let first = acknowledged_last(acks) + 1;
    let mut disk = FileDisk::open(std::path::Path::new(journal)).expect("the journal opens");
    let mut session = JsonlJournal::open(&mut disk).unwrap_or_else(|error| panic!("{error}"));
    if session.dropped_bytes > 0 {
        eprintln!("dropped {} torn bytes", session.dropped_bytes);
    }
    for turn in first..=turns {
        session = match session.run(&mut drill, &request(seed, turn)) {
            Ok((session, Ran::Finished(_) | Ran::AlreadyFinished(_))) => session,
            Err(RunError::Open(error)) => panic!("turn {turn}: {error}"),
            Err(RunError::Exit(exit)) => panic!("turn {turn}: {exit:?}"),
        };
    }
}

fn acknowledged_last(acks: &str) -> u64 {
    let text = std::fs::read_to_string(acks).unwrap_or_default();
    let mut last = 0;
    for line in text.lines() {
        let turn = line
            .split(' ')
            .next()
            .and_then(|turn| turn.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("ack line {line:?} does not start with its turn"));
        last = last.max(turn);
    }
    last
}

struct Logged {
    tool: String,
    done: bool,
}

struct Calls {
    external: BTreeMap<String, Logged>,
    other: BTreeMap<String, Logged>,
    unknown: BTreeSet<String>,
    statuses: BTreeMap<u64, Vec<(String, String)>>,
}

fn calls(records: &[Record]) -> Calls {
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
            } => {
                let map = if *effect == EffectClass::External {
                    &mut calls.external
                } else {
                    &mut calls.other
                };
                let logged = Logged {
                    tool: tool.clone(),
                    done: false,
                };
                if map.insert(call_id(*turn, *seq), logged).is_some() {
                    panic!("seq_unique: call {} recorded twice", call_id(*turn, *seq));
                }
            }
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
            } => {
                calls
                    .statuses
                    .entry(*turn)
                    .or_default()
                    .push((outcome.name().to_owned(), result.encode()));
            }
            _ => {}
        }
    }
    calls
}

struct Dispatch {
    nested: bool,
    id: String,
    tool: String,
    external: bool,
}

fn ledger_runs(ledger: &str) -> Vec<Vec<Dispatch>> {
    let text = std::fs::read_to_string(ledger).unwrap_or_default();
    let mut runs: Vec<Vec<Dispatch>> = Vec::new();
    for line in text.lines() {
        if line == RUN_MARKER {
            runs.push(Vec::new());
            continue;
        }
        let mut parts = line.split(' ');
        let (Some(id), Some(tool), Some(effect), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            panic!("ledger line {line:?} is not 'id tool effect'");
        };
        let run = runs
            .last_mut()
            .unwrap_or_else(|| panic!("ledger line {line:?} comes before any run marker"));
        run.push(Dispatch {
            nested: id.contains(harness::NESTED_ID_INFIX),
            id: id.to_owned(),
            tool: tool.to_owned(),
            external: effect == EffectClass::External.name(),
        });
    }
    runs
}

fn check_finished(session: &harness::Session, calls: &Calls, turns: u64) {
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

fn check_external(calls: &Calls, runs: &[Vec<Dispatch>]) {
    let mut dispatched: BTreeMap<&str, u64> = BTreeMap::new();
    for dispatch in runs.iter().flatten().filter(|dispatch| dispatch.external && !dispatch.nested) {
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

fn check_other(calls: &Calls, runs: &[Vec<Dispatch>]) {
    let (finished_run, killed_runs) = runs
        .split_last()
        .expect("the ledger holds the run that finished");
    for dispatch in finished_run.iter().filter(|dispatch| !dispatch.external && !dispatch.nested) {
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
    let ran: BTreeSet<(&str, &str)> = finished_run
        .iter()
        .chain(killed_runs.iter().flatten())
        .filter(|dispatch| !dispatch.external)
        .map(|dispatch| (dispatch.id.as_str(), dispatch.tool.as_str()))
        .collect();
    for (id, logged) in &calls.other {
        assert!(logged.done, "idempotent_known: {id} ends without a result");
        if !BUILTIN_EFFECTS.contains(&logged.tool.as_str()) && program::code_of(&logged.tool).is_none() {
            assert!(
                ran.contains(&(id.as_str(), logged.tool.as_str())),
                "{id} has a result without a dispatch of {}",
                logged.tool
            );
        }
    }
}

fn check_acks(calls: &Calls, acks: &str, turns: u64) -> usize {
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

fn verify(journal: &str, ledger: &str, acks: &str, turns: u64) {
    let mut disk = FileDisk::open(std::path::Path::new(journal)).expect("the journal opens");
    let opened = harness::jsonl_open(&mut disk).expect("the journal reads");
    let session =
        harness::fold(&opened.records).unwrap_or_else(|error| panic!("the journal folds: {error}"));
    let calls = calls(&opened.records);
    check_finished(&session, &calls, turns);
    let runs = ledger_runs(ledger);
    check_external(&calls, &runs);
    check_other(&calls, &runs);
    let acks_count = check_acks(&calls, acks, turns);
    let tail = check_tail(journal, &session);
    let cells = drill_cells::check_cells(&session, &opened.records, &runs);
    let dispatches: usize = runs.iter().map(Vec::len).sum();
    let unrecorded = runs
        .iter()
        .flatten()
        .filter(|dispatch| {
            !dispatch.external
                && !dispatch.nested
                && !calls
                    .other
                    .get(&dispatch.id)
                    .is_some_and(|logged| logged.tool == dispatch.tool)
        })
        .count();
    println!(
        "verified turns={turns} runs={} records={} external={} unknown={} other={} dispatches={dispatches} other_dispatches_lost_in_a_kill={unrecorded} acks={acks_count} {tail} torn_bytes_dropped={} cells={} cells_threw={} nested_external={} nested_unknown={} nested_other={}",
        runs.len(),
        opened.records.len(),
        calls.external.len(),
        calls.unknown.len(),
        calls.other.len(),
        opened.dropped_bytes,
        cells.cells,
        cells.threw,
        cells.external,
        cells.unknown,
        cells.other
    );
}

fn check_tail(journal: &str, whole: &harness::Session) -> String {
    let mut disk = FileDisk::open(std::path::Path::new(journal)).expect("the journal opens");
    let tail = harness::jsonl_open_tail(&mut disk).expect("the journal's tail reads");
    assert_eq!(tail.dropped_bytes, 0, "the whole open already dropped the torn tail");
    let Start::Checkpoint { revision, next_cell } = tail.start else {
        assert!(
            whole.turns.len() <= 1,
            "a journal of finished turns has a trusted checkpoint"
        );
        return "tail_from=beginning".to_owned();
    };
    let session = harness::fold_from(tail.start, &tail.records)
        .unwrap_or_else(|error| panic!("the tail folds: {error}"));
    for (turn, log) in &session.turns {
        assert_eq!(
            whole.turns.get(turn),
            Some(log),
            "the tail folds turn {turn} as the whole journal does"
        );
    }
    assert_eq!(
        session.turns.first_key_value().map(|(turn, _)| *turn),
        Some(revision + 1),
        "the tail holds the turn after its checkpoint"
    );
    assert_eq!(
        session.turns.last_key_value().map(|(turn, _)| *turn),
        whole.turns.last_key_value().map(|(turn, _)| *turn),
        "the tail reaches the last turn"
    );
    assert_eq!(
        session.checkpoints.get(&revision),
        whole.checkpoints.get(&revision),
        "the tail starts at the whole journal's checkpoint"
    );
    let cells_before = whole
        .cell_origins
        .iter()
        .filter(|(_, (turn, _))| *turn <= revision)
        .map(|(cell, _)| cell + 1)
        .max()
        .unwrap_or(harness::FIRST_CELL);
    assert_eq!(next_cell, cells_before, "the tail's next cell follows every cell before its checkpoint");
    assert_eq!(session.next_cell(), whole.next_cell(), "the tail and the whole journal agree on the next cell");
    let older = harness::jsonl_open_for(&mut disk, revision).expect("the journal reads for an older turn");
    assert_eq!(older.start, Start::Beginning, "an older turn reads the whole journal");
    format!("tail_from={revision} tail_records={}", tail.records.len())
}

fn open_timing(journal: &str, repeats: u64) {
    assert!(repeats > 0, "an open is timed at least once");
    for mode in ["whole", "tail"] {
        let started = std::time::Instant::now();
        let mut bytes_read = 0;
        let mut records = 0;
        for _ in 0..repeats {
            let mut disk = FileDisk::open(std::path::Path::new(journal)).expect("the journal opens");
            let opened = match mode {
                "whole" => harness::jsonl_open(&mut disk),
                _ => harness::jsonl_open_tail(&mut disk),
            }
            .expect("the journal reads");
            let session =
                harness::fold_from(opened.start, &opened.records).expect("the journal folds");
            let next = match session.turns.last_key_value() {
                Some((turn, log)) if log.finished.is_none() => *turn,
                Some((turn, _)) => turn + 1,
                None => session.start.first_turn(),
            };
            harness::state_before(&session, next).expect("the state before the next turn replays");
            bytes_read = disk.bytes_read;
            records = opened.records.len();
        }
        println!(
            "open mode={mode} records={records} bytes_read={bytes_read} ms_per_open={:.3}",
            started.elapsed().as_secs_f64() * 1000.0 / repeats as f64
        );
    }
}

struct PerRecord(FileDisk);

impl Disk for PerRecord {
    fn length(&mut self) -> std::io::Result<u64> {
        self.0.length()
    }

    fn read_at(&mut self, offset: u64, length: u64) -> std::io::Result<Vec<u8>> {
        self.0.read_at(offset, length)
    }

    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        for line in bytes.split_inclusive(|byte| *byte == harness::LINE_END) {
            self.0.append(line)?;
            self.0.sync()?;
        }
        Ok(())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn truncate(&mut self, length: u64) -> std::io::Result<()> {
        self.0.truncate(length)
    }
}

fn bench(journal: &str, ledger: &str, acks: &str, turns: u64, mode: &str) {
    let mut drill = Drill {
        ledger: open_append(ledger),
        acks: open_append(acks),
        rng: Rng(turns),
        clock: 0,
        engine: drill_cells::ScriptEngine::new(journal),
    };
    let started = std::time::Instant::now();
    let file = FileDisk::open(std::path::Path::new(journal)).expect("the journal opens");
    let (syncs, appends, records) = match mode {
        "group" => {
            let mut disk = file;
            for turn in 1..=turns {
                harness::run_jsonl(&mut drill, &mut disk, &request(0, turn))
                    .expect("a bench turn runs");
            }
            let records = harness::jsonl_open(&mut disk)
                .expect("the journal reads")
                .records
                .len();
            (disk.syncs, disk.appends, records)
        }
        "per-record" => {
            let mut disk = PerRecord(file);
            for turn in 1..=turns {
                harness::run_jsonl(&mut drill, &mut disk, &request(0, turn))
                    .expect("a bench turn runs");
            }
            let records = harness::jsonl_open(&mut disk)
                .expect("the journal reads")
                .records
                .len();
            (disk.0.syncs, disk.0.appends, records)
        }
        other => panic!("unknown bench mode {other}"),
    };
    let elapsed = started.elapsed();
    println!(
        "bench mode={mode} turns={turns} records={records} journal_fsyncs={syncs} writes={appends} fsyncs_per_turn={:.2} records_per_turn={:.2} ms_per_turn={:.3}",
        syncs as f64 / turns as f64,
        records as f64 / turns as f64,
        elapsed.as_secs_f64() * 1000.0 / turns as f64
    );
}

fn main() {
    harness::self_check();
    harness::jsonl_self_check();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let number = |text: &str| -> u64 {
        text.parse()
            .unwrap_or_else(|_| panic!("{text} is not a number"))
    };
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["run", journal, ledger, acks, seed, turns] => {
            run(journal, ledger, acks, number(seed), number(turns))
        }
        ["verify", journal, ledger, acks, turns] => verify(journal, ledger, acks, number(turns)),
        ["bench", journal, ledger, acks, turns, mode] => {
            bench(journal, ledger, acks, number(turns), mode)
        }
        ["open", journal, repeats] => open_timing(journal, number(repeats)),
        _ => {
            eprintln!(
                "usage: drill_host run JOURNAL LEDGER ACKS SEED TURNS | verify JOURNAL LEDGER ACKS TURNS | bench JOURNAL LEDGER ACKS TURNS group|per-record | open JOURNAL REPEATS"
            );
            std::process::exit(2);
        }
    }
    let _ = program::HARNESS_NAME;
}
