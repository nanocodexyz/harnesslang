#[path = "harness.rs"]
mod harness;
#[path = "store.rs"]
mod store;

use harness::program::{self, Arguments};
use harness::{Finish, Host, Json, ModelRequest, Output, Ran, ToolRequest, TurnRequest};

const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
const SPLITMIX_MIX_FIRST: u64 = 0xbf58_476d_1ce4_e5b9;
const SPLITMIX_MIX_SECOND: u64 = 0x94d0_49bb_1331_11eb;
const BENCH_SEED: u64 = 1;
const STEPS: [&[&str]; 2] = [&["read", "edit", "bash"], &["read", "bash"]];
const TOOL_OUTPUT_BYTES: usize = 512;
const PROMPT_BYTES: usize = 256;
const WORDS: [&str; 12] = [
    "let", "fn", "match", "state", "journal", "turn", "record", "flush", "return", "self", "if",
    "else",
];
const NANOS_PER_MILLI: f64 = 1_000_000.0;

const RUSAGE_SELF: i32 = 0;
const RUSAGE_WORDS: usize = 18;
const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_MILLI: f64 = 1_000.0;

#[repr(C)]
struct Rusage {
    words: [i64; RUSAGE_WORDS],
}

unsafe extern "C" {
    fn getrusage(who: i32, usage: *mut Rusage) -> i32;
}

fn cpu_ms() -> f64 {
    let mut usage = Rusage {
        words: [0; RUSAGE_WORDS],
    };
    let status = unsafe { getrusage(RUSAGE_SELF, &mut usage) };
    assert_eq!(status, 0, "getrusage of this process succeeds");
    let [user_seconds, user_micros, system_seconds, system_micros, ..] = usage.words;
    let micros = (user_seconds + system_seconds) * MICROS_PER_SECOND + user_micros + system_micros;
    micros as f64 / MICROS_PER_MILLI
}

pub struct Rng(u64);

impl Rng {
    fn draw(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(SPLITMIX_GAMMA);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(SPLITMIX_MIX_FIRST);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(SPLITMIX_MIX_SECOND);
        mixed ^ (mixed >> 31)
    }

    fn text(&mut self, prefix: &str, bytes: usize) -> String {
        let mut text = String::with_capacity(bytes + prefix.len());
        text.push_str(prefix);
        for _ in 0..bytes {
            if text.len() >= bytes {
                break;
            }
            text.push(' ');
            text.push_str(WORDS[(self.draw() % WORDS.len() as u64) as usize]);
        }
        text.truncate(bytes.max(prefix.len()));
        text
    }
}

pub struct Bench {
    rng: Rng,
    turn: u64,
    step: usize,
    clock: u64,
    acknowledged: u64,
    tools: u64,
}

impl Host for Bench {
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        self.tools += 1;
        let id = harness::call_id(request.turn, request.seq);
        Output::Ok(Json::String(
            self.rng
                .text(&format!("{} {id}:", request.tool), TOOL_OUTPUT_BYTES),
        ))
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        if request.turn != self.turn {
            self.turn = request.turn;
            self.step = 0;
        }
        let step = self.step;
        self.step += 1;
        let Some(tools) = STEPS.get(step) else {
            return Json::object(vec![(
                "answer",
                Json::String(format!("done-{}-{}", request.turn, request.seq)),
            )]);
        };
        assert!(
            tools.len() as u64 <= request.calls_max,
            "a scripted step asks for no more calls than the model may"
        );
        let calls = tools
            .iter()
            .enumerate()
            .map(|(index, tool)| {
                Json::object(vec![
                    (
                        "id",
                        Json::String(format!("c{}-{}-{index}", request.turn, request.seq)),
                    ),
                    ("tool", Json::text(tool)),
                    ("input", Json::String(format!("src/file_{index}.rs"))),
                ])
            })
            .collect();
        Json::object(vec![("calls", Json::Array(calls))])
    }

    fn now(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn random(&mut self) -> u64 {
        self.rng.draw()
    }

    fn acknowledge(&mut self, _finish: &Finish) {
        self.acknowledged += 1;
    }
}

fn request(rng: &mut Rng, turn: u64) -> TurnRequest {
    TurnRequest {
        turn,
        arguments: Arguments::Prompt(rng.text(&format!("prompt {turn}:"), PROMPT_BYTES)),
    }
}

fn milliseconds(elapsed: std::time::Duration) -> f64 {
    elapsed.as_nanos() as f64 / NANOS_PER_MILLI
}

fn run(path: &std::path::Path, turns: u64) {
    let mut bench = Bench {
        rng: Rng(BENCH_SEED),
        turn: 0,
        step: 0,
        clock: 0,
        acknowledged: 0,
        tools: 0,
    };
    let mut prompts = Rng(BENCH_SEED.wrapping_add(SPLITMIX_GAMMA));
    let cpu_started = cpu_ms();
    let started = std::time::Instant::now();
    let mut session = store::Session::create(path);
    for turn in 1..=turns {
        match session.turn(&mut bench, &request(&mut prompts, turn)) {
            Ran::Finished(_) => {}
            Ran::AlreadyFinished(finish) => {
                panic!("turn {turn} of a fresh session was already finished: {finish:?}")
            }
        }
    }
    let wall = started.elapsed();
    let cpu = cpu_ms() - cpu_started;
    assert_eq!(bench.acknowledged, turns, "every turn is acknowledged once");
    println!(
        "run turns={turns} wall_ms={:.3} cpu_ms={cpu:.3} tools={} {}",
        milliseconds(wall),
        bench.tools,
        session.report(path)
    );
}

fn open(path: &std::path::Path, turns: u64) {
    let cpu_started = cpu_ms();
    let started = std::time::Instant::now();
    let opened = store::open(path);
    let session =
        harness::fold(&opened.records).unwrap_or_else(|error| panic!("the journal folds: {error}"));
    let next = session
        .turns
        .last_key_value()
        .map_or(1, |(last, _)| last + 1);
    let state = harness::state_before(&session, next)
        .unwrap_or_else(|exit| panic!("the state before turn {next} builds: {exit:?}"));
    let elapsed = started.elapsed();
    let cpu = cpu_ms() - cpu_started;
    let encoded = program::state_json(&state).encode();
    for turn in 1..=turns {
        let finished = session
            .turns
            .get(&turn)
            .is_some_and(|log| log.finished.is_some());
        assert!(finished, "turn {turn} is finished in the journal");
    }
    assert_eq!(
        next,
        turns + 1,
        "the journal holds exactly the bench's turns"
    );
    println!(
        "open open_ms={:.3} open_cpu_ms={cpu:.3} records={} state_bytes={} {}",
        milliseconds(elapsed),
        opened.records.len(),
        encoded.len(),
        opened.report
    );
}

fn main() {
    harness::self_check();
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
        ["run", path, turns] => run(std::path::Path::new(path), number(turns)),
        ["open", path, turns] => open(std::path::Path::new(path), number(turns)),
        _ => {
            eprintln!("usage: bench_host run PATH TURNS | open PATH TURNS");
            std::process::exit(2);
        }
    }
}
