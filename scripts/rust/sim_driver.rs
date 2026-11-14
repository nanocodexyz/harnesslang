#[path = "harness.rs"]
mod harness;
#[path = "store.rs"]
mod store;

use std::io::BufRead;

use harness::program;
use harness::{CellEvent, CellPoint, Delivery, EventAnswer, Exit, Finish, Input, Json, ModelRequest, Output, Point, Ran, Record, Resume, Step, Stop, Suspension, ToolRequest, TurnRequest, Wait, World};

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
    pub hooks: u64,
    pub cells: u64,
    pub steps: u64,
    pub nested: u64,
    pub waits: u64,
    pub sleeps: u64,
    pub resumes: u64,
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
            Ok(Ran::Suspended(_)) => "suspended",
            Ok(Ran::NotDue(_)) => "not_due",
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

fn point_fields(point: Point) -> (&'static str, Option<&'static str>, Option<&'static str>) {
    match point {
        Point::BeforeFlush => ("before_flush", None, None),
        Point::AfterFlush => ("after_flush", None, None),
        Point::BeforeRun(kind, effect) => ("before_run", Some(kind.name()), Some(effect.name())),
        Point::AfterRun(kind, effect) => ("after_run", Some(kind.name()), Some(effect.name())),
        Point::BeforeAcknowledge => ("before_acknowledge", None, None),
        Point::Cell(cell @ (CellPoint::BeforeNested(effect) | CellPoint::AfterNested(effect))) => (cell.name(), None, Some(effect.name())),
        Point::Cell(cell) => (cell.name(), None, None),
    }
}

fn bytes_json(data: &[u8]) -> Json {
    Json::Array(data.iter().map(|byte| Json::Number(u64::from(*byte))).collect())
}

fn input_json(input: &Input) -> Json {
    let (tag, body) = match input {
        Input::Issued => ("issued", Json::Null),
        Input::Refused { error } => ("refused", Json::object([("error", Json::text(error))])),
        Input::Result { id, encoded, success } => (
            "result",
            Json::object([("encoded", Json::text(encoded)), ("id", Json::Number(*id)), ("success", Json::Bool(*success))]),
        ),
        Input::Timer { id } => ("timer", Json::object([("id", Json::Number(*id))])),
        Input::Clock { value } => ("clock", Json::object([("value", Json::Number(*value))])),
        Input::Monotonic { value } => ("monotonic", Json::object([("value", Json::Number(*value))])),
        Input::Random { value } => ("random", Json::object([("value", Json::Fraction(*value))])),
        Input::Bytes { data } => ("bytes", Json::object([("data", bytes_json(data))])),
    };
    Json::object([(tag, body)])
}

fn event_json(event: &CellEvent) -> Json {
    match event {
        CellEvent::Delivered { cell, id } => Json::object([("delivered", Json::object([("cell", Json::Number(*cell)), ("id", Json::Number(*id))]))]),
        CellEvent::Diverged { cell, reason } => Json::object([("diverged", Json::object([("cell", Json::Number(*cell)), ("reason", Json::text(reason))]))]),
        CellEvent::Finished { cell, outcome } => Json::object([(
            "finished",
            Json::object([("cell", Json::Number(*cell)), ("output", harness::output_json(&outcome.output()))]),
        )]),
    }
}

fn step_from(json: &Json) -> Step {
    let (tag, body) = json.tagged().unwrap_or_else(|| fail(&format!("a traced step {} has no tag", json.encode())));
    match tag {
        "call" => Step::Call { id: number(body, "id"), name: text(body, "name").to_owned(), input: field(body, "input").clone() },
        "timeout" => Step::Timeout { id: number(body, "id"), delay_ms: number(body, "delay_ms") },
        "clock" => Step::Clock,
        "monotonic" => Step::Monotonic,
        "random" => Step::Random,
        "bytes" => Step::Bytes { length: number(body, "length") },
        "idle" => Step::Idle,
        "done" => Step::Done { output: text(body, "output").to_owned() },
        "threw" => Step::Threw { error: text(body, "error").to_owned() },
        other => fail(&format!("a traced step has an unknown tag {other}")),
    }
}

impl World for Driver {
    fn hook(&mut self, point: Point) -> Result<(), Stop> {
        let event = self.events.next("hook");
        let (name, kind, effect) = point_fields(point);
        expect(&event, "point", &Json::text(name));
        if let Some(kind) = kind {
            expect(&event, "kind", &Json::text(kind));
        }
        if let Some(effect) = effect {
            expect(&event, "effect", &Json::text(effect));
        }
        self.stats.hooks += 1;
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
        expect(&event, "nested", &request.nested.map_or(Json::Null, |nested| Json::Number(nested.id)));
        if request.nested.is_some() {
            self.stats.nested += 1;
        }
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

    fn tools(&mut self, requests: &[ToolRequest<'_>], limit: Option<u64>) -> Result<Vec<(usize, Output)>, Stop> {
        let event = self.events.next("batch");
        expect(&event, "limit", &limit.map_or(Json::Null, Json::Number));
        let Some(Json::Array(order)) = event.get("order") else {
            fail(&format!("batch event {} has no order", event.encode()));
        };
        let order: Vec<usize> = order
            .iter()
            .map(|index| index.as_u64().unwrap_or_else(|| fail("a batch order holds numbers")) as usize)
            .collect();
        if order.len() != requests.len() {
            fail(&format!("the interpreter's batch has {} calls, the generated code's {}", order.len(), requests.len()));
        }
        let width = harness::batch_width(requests.len(), limit);
        let mut outputs: Vec<Option<Output>> = vec![None; requests.len()];
        let mut started = 0;
        let mut completed = Vec::with_capacity(requests.len());
        while started < width {
            self.hook(Point::BeforeRun(harness::Kind::Dispatch, requests[started].effect))?;
            outputs[started] = Some(self.tool(&requests[started]));
            started += 1;
        }
        for index in order {
            let Some(output) = outputs.get_mut(index).and_then(Option::take) else {
                fail(&format!("batch call {index} completes before it started"));
            };
            self.hook(Point::AfterRun(harness::Kind::Dispatch, requests[index].effect))?;
            completed.push((index, output));
            if started < requests.len() {
                self.hook(Point::BeforeRun(harness::Kind::Dispatch, requests[started].effect))?;
                outputs[started] = Some(self.tool(&requests[started]));
                started += 1;
            }
        }
        Ok(completed)
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        let event = self.events.next("model");
        expect(&event, "turn", &Json::Number(request.turn));
        expect(&event, "seq", &Json::Number(request.seq));
        expect(&event, "calls_max", &Json::Number(request.calls_max));
        let traced = event.get("mode").map(|mode| mode.as_str().expect("a traced mode is text"));
        assert_eq!(traced, request.mode, "model mode at {}/{}", request.turn, request.seq);
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

    fn cell_start(&mut self, cell: u64, script: &str) -> Step {
        let event = self.events.next("cell_start");
        expect(&event, "cell", &Json::Number(cell));
        expect(&event, "script", &Json::text(script));
        self.stats.cells += 1;
        step_from(field(&event, "step"))
    }

    fn cell_resume(&mut self, cell: u64, input: Input) -> Step {
        let event = self.events.next("cell_resume");
        expect(&event, "cell", &Json::Number(cell));
        expect(&event, "input", &input_json(&input));
        self.stats.steps += 1;
        step_from(field(&event, "step"))
    }

    fn wait(&mut self, wait: &Wait) -> Result<(), Stop> {
        let event = self.events.next("wait");
        expect(&event, "cell", &Json::Number(wait.cell));
        expect(&event, "timer", &Json::Number(wait.timer));
        expect(&event, "until", &Json::Number(wait.until_ms));
        self.stats.waits += 1;
        Ok(())
    }

    fn observe(&mut self, observed: &CellEvent) {
        let event = self.events.next("cell");
        expect(&event, "observed", &event_json(observed));
    }

    fn acknowledge(&mut self, finish: &Finish) {
        let event = self.events.next("ack");
        expect(&event, "turn", &Json::Number(finish.turn));
        expect(&event, "outcome", &Json::text(finish.outcome.name()));
        expect(&event, "result", &finish.result);
        self.acks += 1;
    }

    fn inbox(&mut self, turn: u64) -> Vec<Delivery> {
        let event = self.events.next("inbox");
        expect(&event, "turn", &Json::Number(turn));
        let Json::Array(items) = field(&event, "answer") else {
            fail(&format!("trace event {} has no list of deliveries", event.encode()));
        };
        items
            .iter()
            .map(|item| harness::delivery_from(item).unwrap_or_else(|error| fail(&error)))
            .collect()
    }

    fn clock(&mut self) -> u64 {
        number(&self.events.next("clock"), "answer")
    }

    fn sleep(&mut self, turn: u64, seq: u64, until_ms: u64) -> Result<u64, Stop> {
        let event = self.events.next("sleep");
        expect(&event, "turn", &Json::Number(turn));
        expect(&event, "seq", &Json::Number(seq));
        expect(&event, "until", &Json::Number(until_ms));
        self.stats.sleeps += 1;
        match text(&event, "answer") {
            "ok" => Ok(number(&event, "clock")),
            "crash" => Err(Stop::Crash),
            other => fail(&format!("unknown sleep answer {other}")),
        }
    }

    fn resume(&mut self, suspension: &Suspension) -> Option<Resume> {
        if self.events.peek().and_then(|event| event.get("event")).and_then(Json::as_str) != Some("resume") {
            return None;
        }
        let event = self.events.next("resume");
        expect(&event, "turn", &Json::Number(suspension.turn));
        expect(&event, "seq", &Json::Number(suspension.seq));
        Some(Resume { key: text(&event, "key").to_owned(), answer: text(&event, "answer").to_owned() })
    }

    fn resumed(&mut self, suspension: &Suspension, _resume: &Resume, matched: bool) {
        let event = self.events.next("resumed");
        expect(&event, "turn", &Json::Number(suspension.turn));
        expect(&event, "seq", &Json::Number(suspension.seq));
        expect(&event, "matched", &Json::Bool(matched));
        self.stats.resumes += 1;
    }

    fn answer(&mut self, turn: u64, position: u64, answers: &[(String, EventAnswer)]) {
        let event = self.events.next("answer");
        expect(&event, "turn", &Json::Number(turn));
        expect(&event, "position", &Json::Number(position));
        let generated = Json::Array(
            answers
                .iter()
                .map(|(id, answer)| Json::object(vec![("answer", Json::text(answer.name())), ("id", Json::text(id))]))
                .collect(),
        );
        expect(&event, "answers", &generated);
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
        "ok backend=rust target={} harness={} seeds={} events={} runs={} tail_opens={} owners={} flushes={} records={} writes={} syncs={} statements={} hooks={} cells={} cell_steps={} nested_calls={} waits={} sleeps={} resumes={}",
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
        stats.statements,
        stats.hooks,
        stats.cells,
        stats.steps,
        stats.nested,
        stats.waits,
        stats.sleeps,
        stats.resumes
    );
}
