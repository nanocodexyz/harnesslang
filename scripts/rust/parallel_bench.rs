#[path = "harness.rs"]
mod harness;

use harness::program::Arguments;
use harness::{ConcurrentTools, FileDisk, Finish, Host, Json, ModelRequest, Output, Ran, ToolRequest, TurnRequest, run_jsonl};
use std::sync::atomic::{AtomicU64, Ordering};

const NANOS_PER_MILLI: f64 = 1_000_000.0;
const BATCH_TOOL: &str = "Read";

struct Bench {
    latency: std::time::Duration,
    calls: u64,
    concurrent: bool,
    acknowledged: u64,
    dispatched: AtomicU64,
    in_flight: AtomicU64,
    in_flight_max: AtomicU64,
}

impl Bench {
    fn run_tool(&self, request: &ToolRequest<'_>) -> Output {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.in_flight_max.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(self.latency);
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.dispatched.fetch_add(1, Ordering::SeqCst);
        Output::Ok(Json::String(format!("{} {}", request.tool, harness::call_id(request.turn, request.seq))))
    }
}

impl ConcurrentTools for Bench {
    fn tool(&self, request: &ToolRequest<'_>) -> Output {
        self.run_tool(request)
    }
}

impl Host for Bench {
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        self.run_tool(request)
    }

    fn concurrent(&self) -> Option<&dyn ConcurrentTools> {
        if self.concurrent { Some(self) } else { None }
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        assert!(self.calls <= request.calls_max, "the batch fits the model's calls bound");
        let calls = (0..self.calls)
            .map(|index| {
                Json::object(vec![
                    ("id", Json::String(format!("c{}-{}-{index}", request.turn, request.seq))),
                    ("tool", Json::text(BATCH_TOOL)),
                    ("input", Json::String(format!("src/file_{index}.rs"))),
                ])
            })
            .collect();
        Json::object(vec![("calls", Json::Array(calls))])
    }

    fn now(&mut self) -> u64 {
        panic!("the bench program reads no clock")
    }

    fn random(&mut self) -> u64 {
        panic!("the bench program draws no randomness")
    }

    fn acknowledge(&mut self, _finish: &Finish) {
        self.acknowledged += 1;
    }
}

fn main() {
    harness::self_check();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, mode, turns, calls, latency_ms] = args.as_slice() else {
        eprintln!("usage: parallel_bench PATH concurrent|serial TURNS CALLS LATENCY_MS");
        std::process::exit(2);
    };
    let number = |text: &str| -> u64 { text.parse().unwrap_or_else(|_| panic!("{text} is not a number")) };
    let turns = number(turns);
    let mut bench = Bench {
        latency: std::time::Duration::from_millis(number(latency_ms)),
        calls: number(calls),
        concurrent: mode == "concurrent",
        acknowledged: 0,
        dispatched: AtomicU64::new(0),
        in_flight: AtomicU64::new(0),
        in_flight_max: AtomicU64::new(0),
    };
    let mut disk = FileDisk::open(std::path::Path::new(path)).unwrap_or_else(|error| panic!("{path}: {error}"));
    let mut walls = Vec::with_capacity(turns as usize);
    for turn in 1..=turns {
        let request = TurnRequest { turn, arguments: Arguments::Ask(format!("question {turn}")) };
        let started = std::time::Instant::now();
        match run_jsonl(&mut bench, &mut disk, &request) {
            Ok(Ran::Finished(_)) => {}
            other => panic!("turn {turn} did not finish: {other:?}"),
        }
        walls.push(started.elapsed().as_nanos() as f64 / NANOS_PER_MILLI);
    }
    assert_eq!(bench.acknowledged, turns, "every turn is acknowledged once");
    assert_eq!(bench.dispatched.load(Ordering::SeqCst), turns * bench.calls, "every call runs once");
    walls.sort_by(|left, right| left.total_cmp(right));
    println!(
        "mode={mode} turns={turns} calls={} latency_ms={latency_ms} median_ms={:.2} min_ms={:.2} max_ms={:.2} in_flight_max={} syncs={}",
        bench.calls,
        walls[walls.len() / 2],
        walls[0],
        walls[walls.len() - 1],
        bench.in_flight_max.load(Ordering::SeqCst),
        disk.syncs
    );
}
