#[path = "sql_drill_checks.rs"]
mod checks;
#[path = "harness.rs"]
mod harness;

use checks::{append_synced, open_append};
use harness::program::{self, Arguments};
use harness::{
    Disk, FileDisk, Finish, Host, Json, JsonlJournal, ModelRequest, Output, Ran, Returns, RunError,
    Start, ToolRequest, TurnRequest, call_id,
};

const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
const SPLITMIX_MIX_FIRST: u64 = 0xbf58_476d_1ce4_e5b9;
const SPLITMIX_MIX_SECOND: u64 = 0x94d0_49bb_1331_11eb;
const PPM: u64 = 1_000_000;
const ANSWER_PPM: u64 = 350_000;
const UNDECLARED_TOOL_PPM: u64 = 250_000;
const TOOL_FAILURE_PPM: u64 = 150_000;
const UNDECLARED_TOOLS: [&str; 2] = ["web_fetch", "spawn_agent"];
const RUN_OWNER: &str = "jsonl";

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

struct Drill {
    ledger: std::fs::File,
    acks: std::fs::File,
    rng: Rng,
    clock: u64,
    tag: String,
}

impl Host for Drill {
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        let id = call_id(request.turn, request.seq);
        let line = checks::dispatch_line(&id, request.tool, request.effect, &self.tag);
        append_synced(&mut self.ledger, &line);
        if self.rng.chance(TOOL_FAILURE_PPM) {
            return Output::Failed(format!("{} failed at {id}", request.tool));
        }
        Output::Ok(match request.returns {
            Returns::U64 => Json::Number(self.rng.draw() >> 32),
            Returns::Bool => Json::Bool(self.rng.draw() & 1 == 1),
            Returns::Text => Json::String(format!("{}-{id}", request.tool)),
            Returns::Values { values, .. } => {
                Json::String(values[(self.rng.draw() % values.len() as u64) as usize].to_owned())
            }
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
        tag: checks::run_tag(RUN_OWNER),
    };
    let run_line = checks::run_line(&drill.tag);
    append_synced(&mut drill.ledger, &run_line);
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
    let finished = checks::finished_line(&drill.tag);
    append_synced(&mut drill.ledger, &finished);
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

fn verify(journal: &str, ledger: &str, acks: &str, turns: u64) {
    let mut disk = FileDisk::open(std::path::Path::new(journal)).expect("the journal opens");
    let opened = harness::jsonl_open(&mut disk).expect("the journal reads");
    let session =
        harness::fold(&opened.records).unwrap_or_else(|error| panic!("the journal folds: {error}"));
    let verified = checks::check_all(&opened.records, ledger, acks, turns, 0);
    let tail = check_tail(journal, &session);
    println!(
        "verified turns={turns} runs={} records={} external={} unknown={} other={} dispatches={} other_dispatches_lost_in_a_kill={} acks={} {tail} torn_bytes_dropped={}",
        verified.runs,
        opened.records.len(),
        verified.external,
        verified.unknown,
        verified.other,
        verified.dispatches,
        verified.other_lost,
        verified.acks,
        opened.dropped_bytes
    );
}

fn check_tail(journal: &str, whole: &harness::Session) -> String {
    let mut disk = FileDisk::open(std::path::Path::new(journal)).expect("the journal opens");
    let tail = harness::jsonl_open_tail(&mut disk).expect("the journal's tail reads");
    assert_eq!(
        tail.dropped_bytes, 0,
        "the whole open already dropped the torn tail"
    );
    let Start::Checkpoint(revision) = tail.start else {
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
    let older =
        harness::jsonl_open_for(&mut disk, revision).expect("the journal reads for an older turn");
    assert_eq!(
        older.start,
        Start::Beginning,
        "an older turn reads the whole journal"
    );
    format!("tail_from={revision} tail_records={}", tail.records.len())
}

fn open_timing(journal: &str, repeats: u64) {
    assert!(repeats > 0, "an open is timed at least once");
    for mode in ["whole", "tail"] {
        let started = std::time::Instant::now();
        let mut bytes_read = 0;
        let mut records = 0;
        for _ in 0..repeats {
            let mut disk =
                FileDisk::open(std::path::Path::new(journal)).expect("the journal opens");
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
        tag: checks::run_tag(RUN_OWNER),
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
