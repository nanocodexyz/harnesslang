#[path = "harness.rs"]
mod harness;
#[path = "store.rs"]
mod store;

use std::io::BufRead;

use harness::program;
use harness::{Exit, Finish, Json, ModelRequest, Output, Point, Ran, Record, Stop, ToolRequest, TurnRequest, World};

pub struct Events {
    lines: std::io::Lines<std::io::BufReader<std::fs::File>>,
    peeked: Option<Json>,
    pub index: u64,
}

impl Events {
    pub fn peek(&mut self) -> Option<&Json> {
        if self.peeked.is_none() {
            let line = self.lines.next()?.unwrap_or_else(|error| fail(&format!("cannot read the trace: {error}")));
            self.index += 1;
            let event = Json::decode(&line).unwrap_or_else(|error| fail(&format!("trace line {}: {error}", self.index)));
            self.peeked = Some(event);
        }
        self.peeked.as_ref()
    }

    pub fn next(&mut self, kind: &str) -> Json {
        self.peek();
        let event = self.peeked.take().unwrap_or_else(|| fail(&format!("the trace ended where the generated code made a {kind} call")));
        if event.get("event").and_then(Json::as_str) != Some(kind) {
            fail(&format!(
                "event {}: the interpreter made {}, the generated code made a {kind} call",
                self.index,
                event.encode()
            ));
        }
        event
    }
}

pub fn fail(message: &str) -> ! {
    println!("FAIL invariant=backend_equal backend=rust: {message}");
    std::process::exit(1)
}

pub fn field<'a>(event: &'a Json, key: &str) -> &'a Json {
    event.get(key).unwrap_or_else(|| fail(&format!("trace event {} lacks {key}", event.encode())))
}

pub fn number(event: &Json, key: &str) -> u64 {
    field(event, key).as_u64().unwrap_or_else(|| fail(&format!("trace event {} has no number {key}", event.encode())))
}

pub fn text<'a>(event: &'a Json, key: &str) -> &'a str {
    field(event, key).as_str().unwrap_or_else(|| fail(&format!("trace event {} has no text {key}", event.encode())))
}

fn expect(event: &Json, key: &str, generated: &Json) {
    if field(event, key) != generated {
        fail(&format!(
            "seed event {}: {key} is {} in the interpreter and {} in the generated code",
            event.encode(),
            field(event, key).encode(),
            generated.encode()
        ));
    }
}

fn unhex(text: &str) -> Vec<u8> {
    let digits = text.as_bytes();
    if digits.len() % 2 != 0 {
        fail("a hex tail has an odd length");
    }
    let nibble = |digit: u8| (digit as char).to_digit(16).unwrap_or_else(|| fail("a hex tail has a bad digit")) as u8;
    digits.chunks(2).map(|pair| nibble(pair[0]) << 4 | nibble(pair[1])).collect()
}

#[derive(Default)]
pub struct Stats {
    pub seeds: u64,
    pub runs: u64,
    pub tail_opens: u64,
    pub flushes: u64,
    pub records: u64,
    pub syncs: u64,
    pub writes: u64,
    pub statements: u64,
    pub owners: u64,
}

pub struct Driver {
    pub events: Events,
    pub opened: Vec<u8>,
    pub request: Option<TurnRequest>,
    pub acks: u64,
    pub stats: Stats,
    pub store: store::State,
}

impl Driver {
    fn opened_bytes(&mut self, event: &Json) -> Vec<u8> {
        let keep = number(event, "keep") as usize;
        if keep > self.opened.len() {
            fail("a run keeps more bytes than the last open read");
        }
        let mut bytes = self.opened[..keep].to_vec();
        bytes.extend(unhex(text(event, "tail")));
        self.opened = bytes.clone();
        bytes
    }

    fn run(&mut self, event: &Json, nested: bool) {
        let bytes = self.opened_bytes(event);
        let epoch = number(event, "epoch");
        let request = self.request.clone().unwrap_or_else(|| fail("a run before any turn"));
        let (start, records, opened_epoch) = store::open(self, epoch, &bytes, request.turn);
        if start != harness::Start::Beginning {
            self.stats.tail_opens += 1;
        }
        if opened_epoch != epoch {
            fail(&format!("the generated store opened epoch {opened_epoch}, the interpreter {epoch}"));
        }
        self.stats.runs += 1;
        let ran = harness::run_turn_from(self, epoch, start, &records, &request);
        let result = match &ran {
            Ok(Ran::Finished(_)) => "finished",
            Ok(Ran::AlreadyFinished(_)) => "already_finished",
            Err(Exit::Stop(Stop::Crash)) => "crash",
            Err(Exit::Stop(Stop::Fenced)) => "fenced",
            Err(Exit::Corrupt(_)) => "corrupt",
        };
        let exit = self.events.next("exit");
        expect(&exit, "result", &Json::text(result));
        if let Ok(Ran::AlreadyFinished(finish)) = &ran {
            self.acknowledge(finish);
        }
        let _ = nested;
    }

    fn seed(&mut self) {
        loop {
            let event = match self.events.peek() {
                Some(event) => event.clone(),
                None => fail("the trace ended inside a seed"),
            };
            match text(&event, "event") {
                "turn" => {
                    self.events.next("turn");
                    let arguments = program::decode_arguments(text(&event, "handler"), field(&event, "arguments"))
                        .unwrap_or_else(|error| fail(&format!("turn arguments do not decode: {error:?}")));
                    self.request = Some(TurnRequest { turn: number(&event, "turn"), arguments });
                }
                "run" => {
                    self.events.next("run");
                    self.run(&event, false);
                }
                "end" => {
                    self.events.next("end");
                    if text(&event, "result") != "ok" {
                        fail(&format!("the interpreter's seed failed: {}", event.encode()));
                    }
                    expect(&event, "acks", &Json::Number(self.acks));
                    return;
                }
                other => fail(&format!("event {}: the interpreter made {other}, the generated code expected a turn, run or end", self.events.index)),
            }
        }
    }
}

fn point_fields(point: Point) -> (&'static str, Option<(&'static str, &'static str)>) {
    match point {
        Point::BeforeFlush => ("before_flush", None),
        Point::AfterFlush => ("after_flush", None),
        Point::BeforeRun(kind, effect) => ("before_run", Some((kind.name(), effect.name()))),
        Point::AfterRun(kind, effect) => ("after_run", Some((kind.name(), effect.name()))),
        Point::BeforeAcknowledge => ("before_acknowledge", None),
    }
}

impl World for Driver {
    fn hook(&mut self, point: Point) -> Result<(), Stop> {
        let event = self.events.next("hook");
        let (name, site) = point_fields(point);
        expect(&event, "point", &Json::text(name));
        if let Some((kind, effect)) = site {
            expect(&event, "kind", &Json::text(kind));
            expect(&event, "effect", &Json::text(effect));
        }
        match text(&event, "answer") {
            "ok" => Ok(()),
            "crash" => Err(Stop::Crash),
            "owner" => {
                let owner = self.events.next("owner");
                self.stats.owners += 1;
                self.run(&owner, true);
                Ok(())
            }
            other => fail(&format!("unknown hook answer {other}")),
        }
    }

    fn flush(&mut self, epoch: u64, records: &[Record]) -> Result<(), Stop> {
        let event = match self.events.peek() {
            Some(event) => event.clone(),
            None => fail("the trace ended where the generated code flushed"),
        };
        if event.get("event").and_then(Json::as_str) == Some("flush") {
            expect(&event, "epoch", &Json::Number(epoch));
        }
        self.stats.flushes += 1;
        self.stats.records += records.len() as u64;
        store::flush(self, epoch, records)
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        let event = self.events.next("tool");
        expect(&event, "turn", &Json::Number(request.turn));
        expect(&event, "seq", &Json::Number(request.seq));
        expect(&event, "tool", &Json::text(request.tool));
        expect(&event, "effect", &Json::text(request.effect.name()));
        expect(&event, "input", request.input);
        expect(&event, "admitted", &request.admitted.map_or(Json::Null, Json::text));
        harness::output_from(field(&event, "output")).unwrap_or_else(|error| fail(&error))
    }

    fn listing(&mut self, request: &harness::ListingRequest<'_>) -> Output {
        let event = self.events.next("listing");
        expect(&event, "turn", &Json::Number(request.turn));
        expect(&event, "seq", &Json::Number(request.seq));
        expect(&event, "listing", &Json::text(request.listing));
        harness::output_from(field(&event, "output")).unwrap_or_else(|error| fail(&error))
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        let event = self.events.next("model");
        expect(&event, "turn", &Json::Number(request.turn));
        expect(&event, "seq", &Json::Number(request.seq));
        expect(&event, "calls_max", &Json::Number(request.calls_max));
        let context = (request.context)().encode();
        expect(&event, "context_crc", &Json::Number(u64::from(harness::crc32(context.as_bytes()))));
        field(&event, "response").clone()
    }

    fn now(&mut self) -> u64 {
        number(&self.events.next("now"), "answer")
    }

    fn random(&mut self) -> u64 {
        number(&self.events.next("random"), "answer")
    }

    fn acknowledge(&mut self, finish: &Finish) {
        let event = self.events.next("ack");
        expect(&event, "turn", &Json::Number(finish.turn));
        expect(&event, "outcome", &Json::text(finish.outcome.name()));
        expect(&event, "result", &finish.result);
        self.acks += 1;
    }
}

fn main() {
    harness::self_check();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path] = args.as_slice() else {
        eprintln!("usage: sim_driver TRACE");
        std::process::exit(2);
    };
    let file = std::fs::File::open(path).unwrap_or_else(|error| fail(&format!("{path}: {error}")));
    let mut driver = Driver {
        events: Events {
            lines: std::io::BufReader::new(file).lines(),
            peeked: None,
            index: 0,
        },
        opened: Vec::new(),
        request: None,
        acks: 0,
        stats: Stats::default(),
        store: store::State::default(),
    };
    while let Some(event) = driver.events.peek().cloned() {
        let seed = driver.events.next("seed");
        expect(&seed, "target", &Json::text(program::TARGET));
        let _ = event;
        driver.opened.clear();
        driver.request = None;
        driver.acks = 0;
        driver.seed();
        driver.stats.seeds += 1;
    }
    let stats = &driver.stats;
    println!(
        "ok backend=rust target={} harness={} seeds={} events={} runs={} tail_opens={} owners={} flushes={} records={} writes={} syncs={} statements={}",
        program::TARGET,
        program::HARNESS_NAME,
        stats.seeds,
        driver.events.index,
        stats.runs,
        stats.tail_opens,
        stats.owners,
        stats.flushes,
        stats.records,
        stats.writes,
        stats.syncs,
        stats.statements
    );
}
