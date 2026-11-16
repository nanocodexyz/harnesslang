use super::decode::Module;
use super::exec::{Instance, Limits};
use super::js::{Json, parse, short};
use super::{Error, Host, Value};
use crate::run::crc::crc32;
use crate::run::side::SIDE_TURN;

pub const DETAIL_CHARACTERS_MAX: usize = 400;
const WORD_BITS: u32 = 32;
const LOW_WORD: u64 = u32::MAX as u64;
const POINTS: [&str; 13] = [
    "before_flush",
    "after_flush",
    "before_run",
    "after_run",
    "before_acknowledge",
    "cell_before_header",
    "cell_after_header",
    "cell_entry",
    "cell_before_nested",
    "cell_after_nested",
    "cell_waiting",
    "cell_before_finished",
    "cell_after_finished",
];
const POINT_CELL_BEFORE_NESTED: i32 = 8;
const POINT_CELL_AFTER_NESTED: i32 = 9;
const KINDS: [&str; 11] = [
    "tool", "dispatch", "model", "now", "random", "listing", "cell", "wait", "hold", "suspend",
    "deny",
];
const EFFECTS: [&str; 3] = ["none", "idempotent", "external"];
const POINT_BEFORE_RUN: i32 = 2;
const POINT_AFTER_RUN: i32 = 3;
const KIND_DISPATCH: i32 = 1;
const IMPORTS: [(&str, Import); 11] = [
    ("fail", Import::Fail),
    ("hook", Import::Hook),
    ("flush", Import::Flush),
    ("keep", Import::Keep),
    ("tool", Import::Tool),
    ("model", Import::Model),
    ("now", Import::Now),
    ("random", Import::Random),
    ("attempt", Import::Attempt),
    ("world", Import::World),
    ("acknowledge", Import::Acknowledge),
];
const EXPORT_ALLOC: &str = "hl_alloc";
const EXPORT_RUN: &str = "hl_run";
const EXPORT_SIDE: &str = "hl_side";

#[derive(Clone, Copy, Debug)]
pub(crate) enum Import {
    Fail,
    Hook,
    Flush,
    Keep,
    Tool,
    Model,
    Now,
    Random,
    Attempt,
    World,
    Acknowledge,
}

#[derive(Debug)]
pub enum Stop {
    Stopped(&'static str),
    Failure(String),
    Proved(&'static str, String),
    Error(String),
}

fn unproved<T>(property: &'static str, message: String) -> Result<T, Stop> {
    Err(Stop::Proved(
        property,
        short(&message, DETAIL_CHARACTERS_MAX),
    ))
}

#[derive(Default)]
pub struct Bounds {
    pub records_per_run: std::collections::BTreeMap<String, u64>,
    pub requests_per_run: std::collections::BTreeMap<String, u64>,
}

fn failure<T>(message: String) -> Result<T, Stop> {
    Err(Stop::Failure(short(&message, DETAIL_CHARACTERS_MAX)))
}

fn same(what: &str, expected: &Json, actual: &Json) -> Result<(), Stop> {
    let (left, right) = (expected.encode(), actual.encode());
    if left != right {
        let at = left
            .bytes()
            .zip(right.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        return failure(format!(
            "{what}: the two differ at byte {at}: the interpreter has ...{}..., the wasm module has ...{}...",
            window(&left, at),
            window(&right, at)
        ));
    }
    Ok(())
}

const WINDOW_BYTES: usize = 240;

fn window(text: &str, at: usize) -> &str {
    let mut start = at.saturating_sub(WINDOW_BYTES);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (at + WINDOW_BYTES).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    &text[start..end]
}

fn index_text<const N: usize>(names: [&str; N], index: i32) -> Json {
    usize::try_from(index)
        .ok()
        .and_then(|index| names.get(index))
        .map_or(Json::Undefined, |name| Json::text(name))
}

#[derive(Default)]
pub struct Stats {
    pub seeds: u64,
    pub events: u64,
    pub runs: u64,
    pub owners: u64,
    pub flushes: u64,
    pub hooks: u64,
    pub world: u64,
    pub instantiate_ms: f64,
    pub run_ms: f64,
    pub ops: u64,
    pub external: u64,
    pub records_max: u64,
    pub requests_max: u64,
    pub bounded_runs: u64,
}

pub struct Loaded {
    pub module: Module,
    pub bytes: usize,
    pub(crate) imports: Vec<Import>,
    pub(crate) alloc: u32,
    pub(crate) run: u32,
    side: Option<u32>,
}

pub fn load(bytes: &[u8]) -> Result<Loaded, String> {
    let module = Module::decode(bytes).map_err(|error| format!("CompileError: {error}"))?;
    let mut imports = Vec::with_capacity(module.imports.len());
    for import in &module.imports {
        let found = IMPORTS
            .iter()
            .find(|(name, _)| import.module == "hl" && *name == import.name);
        match found {
            Some((_, kind)) => imports.push(*kind),
            None => {
                return Err(format!(
                    "LinkError: import {}.{} is not a function",
                    import.module, import.name
                ));
            }
        }
    }
    let export = |name: &str| {
        module
            .export_function(name)
            .ok_or_else(|| format!("TypeError: the module exports no function {name}"))
    };
    let (alloc, run) = (export(EXPORT_ALLOC)?, export(EXPORT_RUN)?);
    Ok(Loaded {
        bytes: bytes.len(),
        imports,
        alloc,
        run,
        side: module.export_function(EXPORT_SIDE),
        module,
    })
}

pub struct Context<'m> {
    loaded: &'m Loaded,
    clock: &'m dyn Fn() -> f64,
    bounds: &'m Bounds,
    pub stats: Stats,
}

impl<'m> Context<'m> {
    pub fn new(loaded: &'m Loaded, clock: &'m dyn Fn() -> f64, bounds: &'m Bounds) -> Context<'m> {
        Context {
            loaded,
            clock,
            bounds,
            stats: Stats::default(),
        }
    }
}

pub struct Seed<'e> {
    events: &'e [Json],
    index: usize,
    opened: Vec<u8>,
    request: Option<String>,
    acks: u64,
    scope: Json,
    deadlines: std::collections::BTreeMap<String, Json>,
    pending_deadlines: std::collections::BTreeMap<String, Json>,
    handler: String,
    external: std::collections::BTreeMap<String, (u64, i64)>,
    epoch: i64,
    unflushed: Option<String>,
    run_records: u64,
    run_requests: u64,
    run_bounded: bool,
}

impl<'e> Seed<'e> {
    fn begin_run(&mut self, context: &mut Context, epoch: i64) {
        self.epoch = epoch;
        self.unflushed = None;
        self.run_records = 0;
        self.run_requests = 0;
        let records = context.bounds.records_per_run.contains_key(&self.handler);
        let requests = context.bounds.requests_per_run.contains_key(&self.handler);
        self.run_bounded = records || requests;
        if self.run_bounded {
            context.stats.bounded_runs += 1;
        }
    }

    fn requested(&mut self, context: &mut Context) -> Result<(), Stop> {
        self.run_requests += 1;
        context.stats.requests_max = context.stats.requests_max.max(self.run_requests);
        if let Some(bound) = context.bounds.requests_per_run.get(&self.handler)
            && self.run_requests > *bound
        {
            return unproved(
                "worst_case",
                format!(
                    "handler {}: the wasm module made {} outbound requests in one run, over the {bound} hl prove proved",
                    self.handler, self.run_requests
                ),
            );
        }
        Ok(())
    }

    fn flushed(&mut self, context: &mut Context, bytes: &str) -> Result<(), Stop> {
        self.unflushed = None;
        self.run_records += bytes.lines().filter(|line| !line.is_empty()).count() as u64;
        context.stats.records_max = context.stats.records_max.max(self.run_records);
        if let Some(bound) = context.bounds.records_per_run.get(&self.handler)
            && self.run_records > *bound
        {
            return unproved(
                "worst_case",
                format!(
                    "handler {}: the wasm module flushed {} journal records in one run, over the {bound} hl prove proved",
                    self.handler, self.run_records
                ),
            );
        }
        Ok(())
    }

    fn dispatching(&mut self, context: &mut Context, request: &Json) -> Result<(), Stop> {
        if !request.get("effect").is_text("external") {
            return Ok(());
        }
        let call = format!(
            "{}/{} {}",
            request.get("turn").concat(),
            request.get("seq").concat(),
            request.get("tool").concat()
        );
        if let Some(unflushed) = &self.unflushed {
            return unproved(
                "durability",
                format!(
                    "the wasm module dispatched external call {call} before it flushed {unflushed}"
                ),
            );
        }
        let key = format!("{call} {}", request.get("nested").or_null().encode());
        let (count, claimed) = self.external.entry(key).or_insert((0, self.epoch));
        if self.epoch < *claimed {
            return Ok(());
        }
        *claimed = self.epoch;
        *count += 1;
        context.stats.external += 1;
        if *count > 1 {
            return unproved(
                "external_once",
                format!(
                    "the wasm module dispatched external call {call} {} times",
                    *count
                ),
            );
        }
        Ok(())
    }

    fn answered(&mut self, what: String) {
        self.unflushed = Some(what);
    }
    pub fn new(events: &'e [Json]) -> Seed<'e> {
        Seed {
            events,
            index: 0,
            opened: Vec::new(),
            request: None,
            acks: 0,
            scope: Json::Undefined,
            deadlines: std::collections::BTreeMap::new(),
            pending_deadlines: std::collections::BTreeMap::new(),
            handler: String::new(),
            external: std::collections::BTreeMap::new(),
            epoch: 0,
            unflushed: None,
            run_records: 0,
            run_requests: 0,
            run_bounded: false,
        }
    }

    fn keep_deadlines(&mut self) {
        let pending = std::mem::take(&mut self.pending_deadlines);
        self.deadlines.extend(pending);
    }

    fn deadline(&mut self, context: &mut Context, request: &Json) -> Result<Json, Stop> {
        let key = format!(
            "{}/{}/{}",
            request.get("turn").concat(),
            request.get("site").concat(),
            request.get("cell").concat()
        );
        if let Some(clock) = self
            .deadlines
            .get(&key)
            .or_else(|| self.pending_deadlines.get(&key))
        {
            return Ok(clock.clone());
        }
        let clock = self
            .next(context, "now", "a cell deadline")?
            .get("answer")
            .clone();
        self.pending_deadlines.insert(key, clock.clone());
        Ok(clock)
    }

    fn next(&mut self, context: &mut Context, kind: &str, detail: &str) -> Result<&'e Json, Stop> {
        let event = self.events.get(self.index);
        let Some(event) = event.filter(|event| event.get("event").is_text(kind)) else {
            let found = event.map_or_else(|| "nothing more".to_owned(), Json::encode);
            return failure(format!(
                "event {}: the wasm module made a {kind} call ({detail}) where the interpreter made {found}",
                self.index
            ));
        };
        self.index += 1;
        context.stats.events += 1;
        Ok(event)
    }

    fn opened_bytes(&mut self, event: &Json) -> Result<Vec<u8>, Stop> {
        let keep = match event.get("keep") {
            Json::Int(digits) => digits.parse::<usize>().ok(),
            _ => None,
        };
        let Some(keep) = keep.filter(|keep| *keep <= self.opened.len()) else {
            return failure("a run keeps more bytes than the last open read".to_owned());
        };
        let tail = match event.get("tail") {
            Json::Text(hex) => decode_hex(hex),
            _ => Vec::new(),
        };
        self.opened.truncate(keep);
        self.opened.extend_from_slice(&tail);
        Ok(self.opened.clone())
    }

    fn run(&mut self, context: &mut Context, event: &Json) -> Result<(), Stop> {
        self.pending_deadlines.clear();
        let journal = self.opened_bytes(event)?;
        let started = (context.clock)();
        let mut instance = Instance::new(&context.loaded.module, Limits::NODE);
        context.stats.instantiate_ms += (context.clock)() - started;
        context.stats.runs += 1;
        let ran = (context.clock)();
        let request = self.request.clone().unwrap_or_else(|| "null".to_owned());
        let epoch = event.get("epoch").bigint().map_err(Stop::Error)?;
        self.begin_run(context, epoch);
        let mut host = World {
            seed: self,
            context,
        };
        let exit = match host.run(&mut instance, &request, &journal, epoch) {
            Ok(exit) => exit,
            Err(Error::Stop(Stop::Stopped(exit))) => {
                Json::Object(vec![("exit".to_owned(), Json::text(exit))])
            }
            Err(Error::Stop(stop)) => return Err(stop),
            Err(Error::Trap(trap)) => return Err(Stop::Error(format!("RuntimeError: {trap}"))),
        };
        let World { seed, context } = host;
        context.stats.run_ms += (context.clock)() - ran;
        context.stats.ops += instance.executed();
        let result = exit.get("exit");
        if result.is_text("corrupt") {
            return failure(format!(
                "the wasm module found the journal corrupt: {}",
                exit.get("error").concat()
            ));
        }
        let done = seed.next(context, "exit", &result.concat())?;
        same("run exit", done.get("result"), result)?;
        if result.is_text("already_finished") {
            seed.acknowledged(context, exit.get("finish"))?;
        }
        Ok(())
    }

    fn side(&mut self, context: &mut Context, event: &Json) -> Result<(), Stop> {
        let journal = self.opened_bytes(event)?;
        let mut instance = Instance::new(&context.loaded.module, Limits::NODE);
        context.stats.runs += 1;
        let epoch = event.get("epoch").bigint().map_err(Stop::Error)?;
        self.begin_run(context, epoch);
        let mut host = World {
            seed: self,
            context,
        };
        match host.side(&mut instance, &journal, epoch) {
            Ok(()) => Ok(()),
            Err(Error::Stop(Stop::Stopped(exit))) => {
                Err(Stop::Error(format!("a {exit} stop escaped a side run")))
            }
            Err(Error::Stop(stop)) => Err(stop),
            Err(Error::Trap(trap)) => Err(Stop::Error(format!("RuntimeError: {trap}"))),
        }
    }

    fn acknowledged(&mut self, context: &mut Context, finish: &Json) -> Result<(), Stop> {
        let event = self.next(context, "ack", &finish.encode())?;
        same("acknowledged turn", event.get("turn"), finish.get("turn"))?;
        same(
            "acknowledged outcome",
            event.get("outcome"),
            finish.get("outcome"),
        )?;
        same(
            "acknowledged result",
            event.get("result"),
            finish.get("result"),
        )?;
        self.acks += 1;
        Ok(())
    }

    pub fn replay(&mut self, context: &mut Context) -> Result<(), Stop> {
        loop {
            let Some(event) = self.events.get(self.index) else {
                return failure("the trace ended inside a seed".to_owned());
            };
            let kind = event.get("event");
            if kind.is_text("turn") {
                self.next(context, "turn", "")?;
                let mut fields = vec![
                    ("arguments".to_owned(), event.get("arguments").clone()),
                    ("handler".to_owned(), event.get("handler").clone()),
                    ("turn".to_owned(), event.get("turn").clone()),
                ];
                if !matches!(self.scope, Json::Undefined) {
                    fields.push(("scope".to_owned(), self.scope.clone()));
                }
                let request = Json::Object(fields);
                self.request = Some(request.encode());
                self.handler = event.get("handler").concat();
            } else if kind.is_text("run") {
                self.next(context, "run", "")?;
                self.run(context, event)?;
            } else if kind.is_text("side_open") {
                self.next(context, "side_open", "")?;
                self.side(context, event)?;
            } else if kind.is_text("end") {
                self.next(context, "end", "")?;
                let result = event.get("result");
                if !result.is_text("ok") {
                    return failure(format!(
                        "the interpreter's seed failed: {}",
                        result.concat()
                    ));
                }
                return same("acknowledgements", event.get("acks"), &Json::int(self.acks));
            } else {
                return failure(format!(
                    "event {}: the interpreter made {}, the wasm host expected a turn, run or end",
                    self.index,
                    kind.concat()
                ));
            }
        }
    }
}

fn decode_hex(hex: &str) -> Vec<u8> {
    let digits: Vec<u8> = hex
        .bytes()
        .map_while(|byte| (byte as char).to_digit(16).map(|digit| digit as u8))
        .collect();
    digits
        .chunks_exact(2)
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect()
}

struct World<'s, 'e, 'm> {
    seed: &'s mut Seed<'e>,
    context: &'s mut Context<'m>,
}

fn i32_arg(args: &[Value], index: usize) -> i32 {
    match args[index] {
        Value::I32(value) => value,
        Value::I64(_) => panic!("import argument {index} is an i32"),
    }
}

fn i64_arg(args: &[Value], index: usize) -> i64 {
    match args[index] {
        Value::I64(value) => value,
        Value::I32(_) => panic!("import argument {index} is an i64"),
    }
}

impl<'e> World<'_, 'e, '_> {
    fn text(&self, instance: &Instance, at: i32, length: i32) -> Result<String, Stop> {
        let bytes = instance
            .memory()
            .read(at as u32, length as u32)
            .map_err(|_| {
                Stop::Error(format!("RangeError: Invalid typed array length: {length}"))
            })?;
        String::from_utf8(bytes.to_vec()).map_err(|_| {
            Stop::Error("TypeError: The encoded data was not valid for encoding utf-8".to_owned())
        })
    }

    fn json(&self, instance: &Instance, args: &[Value]) -> Result<Json, Stop> {
        parse(&self.text(instance, i32_arg(args, 0), i32_arg(args, 1))?).map_err(Stop::Error)
    }

    fn give(&mut self, instance: &mut Instance, bytes: &[u8]) -> Result<u64, Error<Stop>> {
        let length = bytes.len() as u32;
        let allocated = instance.invoke(
            self,
            self.context.loaded.alloc,
            &[Value::I32(length as i32)],
        )?;
        let Some(Value::I32(at)) = allocated else {
            panic!("hl_alloc returns an i32");
        };
        instance.memory_mut().write(at as u32, bytes).map_err(|_| {
            Error::Stop(Stop::Error(
                "RangeError: Start offset is outside the bounds of the buffer".to_owned(),
            ))
        })?;
        Ok((u64::from(at as u32) << WORD_BITS) | u64::from(length))
    }

    fn give_json(
        &mut self,
        instance: &mut Instance,
        value: &Json,
    ) -> Result<Option<Value>, Error<Stop>> {
        let text = match value {
            Json::Undefined => String::new(),
            other => other.encode(),
        };
        self.give(instance, text.as_bytes())
            .map(|packed| Some(Value::I64(packed as i64)))
    }

    fn run(
        &mut self,
        instance: &mut Instance,
        request: &str,
        journal: &[u8],
        epoch: i64,
    ) -> Result<Json, Error<Stop>> {
        let request_at = self.give(instance, request.as_bytes())?;
        let journal_at = self.give(instance, journal)?;
        let words = |packed: u64| {
            [
                Value::I32((packed >> WORD_BITS) as u32 as i32),
                Value::I32((packed & LOW_WORD) as u32 as i32),
            ]
        };
        let [request_pointer, request_length] = words(request_at);
        let [journal_pointer, journal_length] = words(journal_at);
        let args = [
            request_pointer,
            request_length,
            journal_pointer,
            journal_length,
            Value::I64(epoch),
        ];
        let Some(Value::I64(packed)) = instance.invoke(self, self.context.loaded.run, &args)?
        else {
            panic!("hl_run returns an i64");
        };
        let [at, length] = words(packed as u64);
        let (Value::I32(at), Value::I32(length)) = (at, length) else {
            unreachable!("words are i32 values");
        };
        let text = self.text(instance, at, length).map_err(Error::Stop)?;
        parse(&text).map_err(|error| Error::Stop(Stop::Error(error)))
    }

    fn side(
        &mut self,
        instance: &mut Instance,
        journal: &[u8],
        epoch: i64,
    ) -> Result<(), Error<Stop>> {
        let journal_at = self.give(instance, journal)?;
        let Some(side) = self.context.loaded.side else {
            return Err(Error::Stop(Stop::Error(
                "TypeError: this.exports.hl_side is not a function".to_owned(),
            )));
        };
        let args = [
            Value::I32((journal_at >> WORD_BITS) as u32 as i32),
            Value::I32((journal_at & LOW_WORD) as u32 as i32),
            Value::I64(epoch),
        ];
        instance.invoke(self, side, &args).map(|_| ())
    }

    fn hook(&mut self, point: i32, kind: i32, effect: &Json) -> Result<(), Stop> {
        let name = index_text(POINTS, point);
        if point == POINT_BEFORE_RUN
            && kind == KIND_DISPATCH
            && effect.is_text("external")
            && let Some(unflushed) = &self.seed.unflushed
        {
            return unproved(
                "durability",
                format!("the wasm module began an external dispatch before it flushed {unflushed}"),
            );
        }
        let event = self.seed.next(self.context, "hook", &name.concat())?;
        same(
            &format!("hook point at event {}", self.seed.index - 1),
            event.get("point"),
            &name,
        )?;
        if point == POINT_BEFORE_RUN || point == POINT_AFTER_RUN {
            same("hook kind", event.get("kind"), &index_text(KINDS, kind))?;
            same("hook effect", event.get("effect"), effect)?;
        }
        if point == POINT_CELL_BEFORE_NESTED || point == POINT_CELL_AFTER_NESTED {
            same("nested hook effect", event.get("effect"), effect)?;
        }
        self.context.stats.hooks += 1;
        let answer = event.get("answer");
        if answer.is_text("crash") {
            return Err(Stop::Stopped("crash"));
        }
        if answer.is_text("owner") {
            let owner = self.seed.next(self.context, "owner", "a second owner")?;
            self.context.stats.owners += 1;
            let outer = (
                self.seed.epoch,
                self.seed.unflushed.take(),
                self.seed.run_records,
                self.seed.run_requests,
                self.seed.run_bounded,
            );
            let ran = self.seed.run(self.context, owner);
            (
                self.seed.epoch,
                self.seed.unflushed,
                self.seed.run_records,
                self.seed.run_requests,
                self.seed.run_bounded,
            ) = outer;
            return ran;
        }
        if !answer.is_text("ok") {
            return failure(format!("unknown hook answer {}", answer.concat()));
        }
        Ok(())
    }

    fn tool_event(&mut self, request: &Json) -> Result<&'e Json, Stop> {
        let detail = short(&request.encode(), DETAIL_CHARACTERS_MAX);
        self.seed.requested(self.context)?;
        self.seed.dispatching(self.context, request)?;
        let event = self.seed.next(self.context, "tool", &detail)?;
        let checks = [
            ("turn", "turn", "turn"),
            ("seq", "seq", "seq"),
            ("name", "tool", "tool"),
            ("class", "effect", "effect"),
            ("input", "input", "input"),
        ];
        for (what, expected, actual) in checks {
            same(
                &format!("tool call {what}"),
                event.get(expected),
                request.get(actual),
            )?;
        }
        same(
            "tool call admitted",
            event.get("admitted").or_null(),
            request.get("admitted").or_null(),
        )?;
        let job = |json: &Json| matches!(json.get("job"), Json::Bool(true));
        if job(event) != job(request) {
            return failure(format!(
                "tool call job: the interpreter has {}, the wasm module has {}",
                job(event),
                job(request)
            ));
        }
        Ok(event.get("output"))
    }

    fn batch(&mut self, request: &Json) -> Result<Json, Stop> {
        let requests: &[Json] = match request.get("requests") {
            Json::Array(items) => items,
            other => {
                return Err(Stop::Error(format!(
                    "TypeError: Cannot read properties of {} (reading 'length')",
                    other.concat()
                )));
            }
        };
        let count = requests.len();
        let event = self
            .seed
            .next(self.context, "batch", &format!("a batch of {count} calls"))?;
        same(
            "batch limit",
            event.get("limit").or_null(),
            request.get("limit"),
        )?;
        let Json::Array(order) = event.get("order") else {
            return Err(Stop::Error(
                "TypeError: event.order.map is not a function".to_owned(),
            ));
        };
        if order.len() != count {
            return failure(format!(
                "batch size: the interpreter has {}, the wasm module has {count}",
                order.len()
            ));
        }
        let width = match request.get("limit") {
            Json::Null => count,
            Json::Int(digits) => digits
                .parse::<usize>()
                .map_or(count, |limit| limit.min(count)),
            _ => 0,
        };
        let mut outputs: Vec<Option<&'e Json>> = vec![None; count];
        let mut completed = Vec::with_capacity(count);
        let mut started = 0;
        while started < width {
            self.start(requests, &mut outputs, started)?;
            started += 1;
        }
        for index in order {
            let slot = index
                .bigint()
                .ok()
                .and_then(|index| usize::try_from(index).ok());
            let Some(index) = slot.filter(|index| *index < started && outputs[*index].is_some())
            else {
                return failure(format!(
                    "batch call {} completes before it started",
                    index.concat()
                ));
            };
            self.hook(
                POINT_AFTER_RUN,
                KIND_DISPATCH,
                requests[index].get("effect"),
            )?;
            let output = outputs[index].take().expect("a started call has an output");
            completed.push(Json::Array(vec![Json::int(index), output.clone()]));
            if started < count {
                self.start(requests, &mut outputs, started)?;
                started += 1;
            }
        }
        Ok(Json::Object(vec![(
            "completed".to_owned(),
            Json::Array(completed),
        )]))
    }

    fn start(
        &mut self,
        requests: &[Json],
        outputs: &mut [Option<&'e Json>],
        index: usize,
    ) -> Result<(), Stop> {
        self.hook(
            POINT_BEFORE_RUN,
            KIND_DISPATCH,
            requests[index].get("effect"),
        )?;
        outputs[index] = Some(self.tool_event(&requests[index])?);
        Ok(())
    }

    fn flush(&mut self, instance: &Instance, args: &[Value]) -> Result<(), Stop> {
        let epoch = i64_arg(args, 0);
        let bytes = self.text(instance, i32_arg(args, 1), i32_arg(args, 2))?;
        let event = self
            .seed
            .next(self.context, "flush", &short(&bytes, DETAIL_CHARACTERS_MAX))?;
        same("flush epoch", event.get("epoch"), &Json::int(epoch))?;
        if !event.get("bytes").is_text(&bytes) {
            return failure(format!(
                "flushed records: the interpreter has {}, the wasm module has {}",
                event.get("bytes").encode(),
                Json::Text(bytes).encode()
            ));
        }
        self.context.stats.flushes += 1;
        let answer = event.get("answer");
        if answer.is_text("ok") {
            self.seed.flushed(self.context, &bytes)?;
        }
        if answer.is_text("crash") {
            return Err(Stop::Stopped("crash"));
        }
        if answer.is_text("fenced") {
            return Err(Stop::Stopped("fenced"));
        }
        if !answer.is_text("ok") {
            return failure(format!("unknown flush answer {}", answer.concat()));
        }
        self.seed.keep_deadlines();
        Ok(())
    }

    fn tool(
        &mut self,
        instance: &mut Instance,
        args: &[Value],
    ) -> Result<Option<Value>, Error<Stop>> {
        let request = self.json(instance, args).map_err(Error::Stop)?;
        let output = self.tool_event(&request).map_err(Error::Stop)?;
        self.seed.answered(format!(
            "the result of call {}/{}",
            request.get("turn").concat(),
            request.get("seq").concat()
        ));
        self.give_json(instance, output)
    }

    fn model(
        &mut self,
        instance: &mut Instance,
        args: &[Value],
    ) -> Result<Option<Value>, Error<Stop>> {
        let request = self.json(instance, args).map_err(Error::Stop)?;
        if request.get("turn").bigint() == Ok(SIDE_TURN as i64) {
            return self.side_model(instance, &request);
        }
        let detail = format!(
            "{}/{}",
            request.get("turn").concat(),
            request.get("seq").concat()
        );
        self.seed.requested(self.context).map_err(Error::Stop)?;
        let event = self
            .seed
            .next(self.context, "model", &detail)
            .map_err(Error::Stop)?;
        if !matches!(request.get("http"), Json::Undefined) {
            let checked = (|| {
                same("model call turn", event.get("turn"), request.get("turn"))?;
                same("model call seq", event.get("seq"), request.get("seq"))?;
                same("model http request", event.get("http"), request.get("http"))
            })();
            checked.map_err(Error::Stop)?;
            return self.give_json(instance, event.get("answer"));
        }
        let checked = (|| {
            same("model call turn", event.get("turn"), request.get("turn"))?;
            same("model call seq", event.get("seq"), request.get("seq"))?;
            same(
                "model calls_max",
                event.get("calls_max"),
                request.get("calls_max"),
            )?;
            same(
                "model mode",
                event.get("mode").or_null(),
                request.get("mode").or_null(),
            )?;
            same(
                "model settings",
                event.get("settings").or_null(),
                request.get("settings").or_null(),
            )?;
            let context = request.get("context");
            let encoded = match context {
                Json::Undefined => String::new(),
                other => other.encode(),
            };
            same(
                "model context crc",
                event.get("context_crc"),
                &Json::int(crc32(encoded.as_bytes())),
            )
        })();
        checked.map_err(Error::Stop)?;
        self.seed.answered(format!("the model response {detail}"));
        self.give_json(instance, event.get("response"))
    }

    fn side_model(
        &mut self,
        instance: &mut Instance,
        request: &Json,
    ) -> Result<Option<Value>, Error<Stop>> {
        let detail = format!("side {}", request.get("seq").concat());
        let event = self
            .seed
            .next(self.context, "side_model", &detail)
            .map_err(Error::Stop)?;
        let encoded = match request.get("context") {
            Json::Undefined => String::new(),
            other => other.encode(),
        };
        let checked = (|| {
            same("side_model seq", event.get("seq"), request.get("seq"))?;
            same(
                "side_model mode",
                event.get("mode").or_null(),
                request.get("mode").or_null(),
            )?;
            same(
                "side_model settings",
                event.get("settings").or_null(),
                request.get("settings").or_null(),
            )?;
            same(
                "side_model context_crc",
                event.get("context_crc"),
                &Json::int(crc32(encoded.as_bytes())),
            )
        })();
        checked.map_err(Error::Stop)?;
        self.context.stats.world += 1;
        self.give_json(instance, event.get("response"))
    }

    fn answer(&mut self, kind: &str) -> Result<Option<Value>, Stop> {
        let event = self.seed.next(self.context, kind, kind)?;
        Ok(Some(Value::I64(
            event.get("answer").bigint().map_err(Stop::Error)?,
        )))
    }

    fn attempt(&mut self, args: &[Value]) -> Result<Option<Value>, Stop> {
        let (turn, seq) = (i64_arg(args, 0), i64_arg(args, 1));
        let event = self
            .seed
            .next(self.context, "attempt", &format!("{turn}/{seq}"))?;
        same("attempt turn", event.get("turn"), &Json::int(turn))?;
        same("attempt seq", event.get("seq"), &Json::int(seq))?;
        Ok(Some(Value::I64(
            event.get("answer").bigint().map_err(Stop::Error)?,
        )))
    }

    fn world(
        &mut self,
        instance: &mut Instance,
        args: &[Value],
    ) -> Result<Option<Value>, Error<Stop>> {
        let request = self.json(instance, args).map_err(Error::Stop)?;
        if matches!(request.get("optional"), Json::Bool(true)) {
            let peeked = self
                .seed
                .events
                .get(self.seed.index)
                .map(|event| event.get("event"));
            let matched = match (peeked, request.get("event")) {
                (Some(Json::Text(found)), Json::Text(kind)) => found == kind,
                _ => false,
            };
            if !matched {
                return self.give_json(instance, &Json::Null);
            }
        }
        if request.get("event").is_text("batch") {
            let answer = self.batch(&request).map_err(Error::Stop)?;
            self.seed.answered("the results of a batch".to_owned());
            return self.give_json(instance, &answer);
        }
        if request.get("event").is_text("deadline") {
            let clock = self
                .seed
                .deadline(self.context, &request)
                .map_err(Error::Stop)?;
            let answer = Json::Object(vec![("answer".to_owned(), clock)]);
            return self.give_json(instance, &answer);
        }
        if request.get("event").is_text("wait") {
            self.seed.keep_deadlines();
        }
        let event = self.world_event(&request).map_err(Error::Stop)?;
        self.give_json(instance, event)
    }

    fn world_event(&mut self, request: &Json) -> Result<&'e Json, Stop> {
        if matches!(request, Json::Null | Json::Undefined) {
            return Err(Stop::Error(format!(
                "TypeError: Cannot read properties of {} (reading 'event')",
                request.concat()
            )));
        }
        let kind = request.get("event").concat();
        let event = self.seed.next(
            self.context,
            &kind,
            &short(&request.encode(), DETAIL_CHARACTERS_MAX),
        )?;
        for key in request.keys() {
            if key != "event" && key != "optional" {
                same(
                    &format!("{kind} {key} at event {}", self.seed.index - 1),
                    event.get(key).or_null(),
                    request.get(key),
                )?;
            }
        }
        self.context.stats.world += 1;
        let answer = event.get("answer");
        if answer.is_text("crash") {
            return Err(Stop::Stopped("crash"));
        }
        if answer.is_text("fenced") {
            return Err(Stop::Stopped("fenced"));
        }
        Ok(event)
    }
}

impl Host for World<'_, '_, '_> {
    type Stop = Stop;

    fn call(
        &mut self,
        import: usize,
        args: &[Value],
        instance: &mut Instance<'_>,
    ) -> Result<Option<Value>, Error<Stop>> {
        let stopped = |result: Result<(), Stop>| result.map(|()| None).map_err(Error::Stop);
        match self.context.loaded.imports[import] {
            Import::Fail => {
                let text = self
                    .text(instance, i32_arg(args, 0), i32_arg(args, 1))
                    .map_err(Error::Stop)?;
                Err(Error::Stop(Stop::Failure(format!(
                    "the wasm module failed: {text}"
                ))))
            }
            Import::Hook => {
                let effect = index_text(EFFECTS, i32_arg(args, 2));
                stopped(self.hook(i32_arg(args, 0), i32_arg(args, 1), &effect))
            }
            Import::Flush => stopped(self.flush(instance, args)),
            Import::Keep => Ok(None),
            Import::Tool => self.tool(instance, args),
            Import::Model => self.model(instance, args),
            Import::Now => self.answer("now").map_err(Error::Stop),
            Import::Random => self.answer("random").map_err(Error::Stop),
            Import::Attempt => self.attempt(args).map_err(Error::Stop),
            Import::World => self.world(instance, args),
            Import::Acknowledge => {
                let finish = self.json(instance, args).map_err(Error::Stop)?;
                stopped(self.seed.acknowledged(self.context, &finish))
            }
        }
    }
}

pub enum Outcome {
    Passed(String),
    Failed(String),
    Errored(String),
}

pub fn replay(
    target: &str,
    bytes: &[u8],
    trace: &str,
    clock: &dyn Fn() -> f64,
    bounds: &Bounds,
) -> Outcome {
    let compiled = clock();
    let loaded = match load(bytes) {
        Ok(loaded) => loaded,
        Err(error) => return Outcome::Errored(error),
    };
    let compile_ms = clock() - compiled;
    let mut context = Context::new(&loaded, clock, bounds);
    let mut current: Option<(Json, Vec<Json>)> = None;
    for line in trace.split('\n') {
        if line.is_empty() {
            continue;
        }
        let event = match parse(line) {
            Ok(event) => event,
            Err(error) => return Outcome::Errored(error),
        };
        if event.get("event").is_text("seed") {
            if let Some((header, events)) = current.take()
                && let Err(outcome) = replay_seed(target, &mut context, &header, &events)
            {
                return outcome;
            }
            current = Some((event, Vec::new()));
            continue;
        }
        match current.as_mut() {
            Some((_, events)) => events.push(event),
            None => {
                return Outcome::Errored(
                    "TypeError: Cannot read properties of null (reading 'events')".to_owned(),
                );
            }
        }
    }
    if let Some((header, events)) = current.take()
        && let Err(outcome) = replay_seed(target, &mut context, &header, &events)
    {
        return outcome;
    }
    Outcome::Passed(summary(target, &context.stats, loaded.bytes, compile_ms))
}

fn replay_seed(
    target: &str,
    context: &mut Context,
    header: &Json,
    events: &[Json],
) -> Result<(), Outcome> {
    if !header.get("target").is_text(target) {
        return Err(Outcome::Errored(format!(
            "Error: the trace is for {}, not {target}",
            header.get("target").concat()
        )));
    }
    let mut seed = Seed::new(events);
    seed.scope = header.get("scope").clone();
    let seed_name = header.get("seed").concat();
    match seed.replay(context) {
        Ok(()) => {}
        Err(Stop::Failure(message)) => {
            return Err(Outcome::Failed(format!(
                "FAIL invariant=backend_equal seed={seed_name} target={target}: {message}"
            )));
        }
        Err(Stop::Proved(property, message)) => {
            return Err(Outcome::Failed(format!(
                "FAIL invariant=proved_{property} seed={seed_name} target={target}: {message}"
            )));
        }
        Err(Stop::Error(message)) => {
            return Err(Outcome::Errored(format!("seed {seed_name}: {message}")));
        }
        Err(Stop::Stopped(exit)) => {
            return Err(Outcome::Errored(format!(
                "seed {seed_name}: a {exit} stop escaped its run"
            )));
        }
    }
    context.stats.seeds += 1;
    Ok(())
}

fn summary(target: &str, stats: &Stats, module_bytes: usize, compile_ms: f64) -> String {
    let per_run_us = stats.instantiate_ms * 1000.0 / stats.runs.max(1) as f64;
    format!(
        "ok backend=wasm target={target} seeds={} events={} runs={} owners={} flushes={} hooks={} world={} module_bytes={module_bytes} compile_ms={compile_ms:.1} instantiate_us_per_run={per_run_us:.1} run_ms={:.1} ops_per_run={} external_once={} records_per_run_max={} requests_per_run_max={} bounded_runs={}",
        stats.seeds,
        stats.events,
        stats.runs,
        stats.owners,
        stats.flushes,
        stats.hooks,
        stats.world,
        stats.run_ms,
        stats.ops / stats.runs.max(1),
        stats.external,
        stats.records_max,
        stats.requests_max,
        stats.bounded_runs
    )
}
