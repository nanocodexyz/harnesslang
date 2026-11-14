use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::ir::{Code, Program};
use crate::run::cell::{CellEngine, Input, Step};
use crate::run::json::{self, Json};
use crate::run::value;
use crate::sim::rng::Rng;

pub const CELL_CALL_PPM: u64 = 200_000;
pub const OVER_LIMIT_PPM: u64 = 5_000;
pub const OP_CALL_PPM: u64 = 400_000;
pub const OP_PARALLEL_PPM: u64 = 150_000;
pub const OP_SLEEP_PPM: u64 = 100_000;
pub const OP_PRINT_PPM: u64 = 100_000;
pub const END_RETURN_PPM: u64 = 700_000;
pub const END_THROW_PPM: u64 = 100_000;
pub const READ_KINDS: u64 = 3;
pub const SLEEP_MS_MAX: u64 = 1_000;
pub const DIVERGENCE_PPM: u64 = 10_000;

const CATCH: &str = ".catch((error) => error)";
const TOOLS_PREFIX: &str = "tools.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Calls(Vec<(String, Json)>),
    Sleep(u64),
    Clock,
    Monotonic,
    Random,
    Bytes(u64),
    Print(String),
    Throw(String),
    Return(String),
}

fn call_text(name: &str, input: &Json) -> String {
    format!("{TOOLS_PREFIX}{name}({}){CATCH}", input.encode())
}

pub fn op_text(index: usize, op: &Op) -> String {
    match op {
        Op::Calls(calls) if calls.len() == 1 => {
            format!(
                "const r{index} = await {};",
                call_text(&calls[0].0, &calls[0].1)
            )
        }
        Op::Calls(calls) => {
            let items: Vec<String> = calls
                .iter()
                .map(|(name, input)| call_text(name, input))
                .collect();
            format!(
                "const p{index} = await Promise.all([{}]);",
                items.join(", ")
            )
        }
        Op::Sleep(ms) => format!("await new Promise((resolve) => setTimeout(resolve, {ms}));"),
        Op::Clock => format!("const c{index} = Date.now();"),
        Op::Monotonic => format!("const m{index} = performance.now();"),
        Op::Random => format!("const x{index} = Math.random();"),
        Op::Bytes(length) => {
            format!("const b{index} = crypto.getRandomValues(new Uint8Array({length}));")
        }
        Op::Print(text) => format!("console.log({});", Json::text(text).encode()),
        Op::Throw(text) => format!("throw new Error({});", Json::text(text).encode()),
        Op::Return(text) => format!("return {};", Json::text(text).encode()),
    }
}

pub fn script(ops: &[Op]) -> String {
    let lines: Vec<String> = ops
        .iter()
        .enumerate()
        .map(|(index, op)| op_text(index, op))
        .collect();
    lines.join("\n")
}

fn parse_call(text: &str) -> Option<(String, Json)> {
    let rest = text.trim().strip_prefix(TOOLS_PREFIX)?;
    let (name, rest) = rest.split_once('(')?;
    let input = rest.strip_suffix(CATCH)?.strip_suffix(')')?;
    Some((name.to_owned(), json::decode(input).ok()?))
}

fn parse_text(text: &str) -> Option<String> {
    json::decode(text).ok()?.as_str().map(str::to_owned)
}

fn after_binding(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("const ")?;
    let (_, value) = rest.split_once(" = ")?;
    value.strip_suffix(';')
}

pub fn parse_line(line: &str) -> Option<Op> {
    if let Some(ms) = line
        .strip_prefix("await new Promise((resolve) => setTimeout(resolve, ")
        .and_then(|rest| rest.strip_suffix("));"))
    {
        return ms.parse().ok().map(Op::Sleep);
    }
    if let Some(text) = line
        .strip_prefix("console.log(")
        .and_then(|rest| rest.strip_suffix(");"))
    {
        return parse_text(text).map(Op::Print);
    }
    if let Some(text) = line
        .strip_prefix("throw new Error(")
        .and_then(|rest| rest.strip_suffix(");"))
    {
        return parse_text(text).map(Op::Throw);
    }
    if let Some(text) = line
        .strip_prefix("return ")
        .and_then(|rest| rest.strip_suffix(';'))
    {
        return parse_text(text).map(Op::Return);
    }
    let value = after_binding(line)?;
    match value {
        "Date.now()" => return Some(Op::Clock),
        "performance.now()" => return Some(Op::Monotonic),
        "Math.random()" => return Some(Op::Random),
        _ => {}
    }
    if let Some(length) = value
        .strip_prefix("crypto.getRandomValues(new Uint8Array(")
        .and_then(|rest| rest.strip_suffix("))"))
    {
        return length.parse().ok().map(Op::Bytes);
    }
    if let Some(items) = value
        .strip_prefix("await Promise.all([")
        .and_then(|rest| rest.strip_suffix("])"))
    {
        let mut calls = Vec::new();
        for piece in items.split_inclusive(CATCH) {
            let piece = piece.trim_start_matches(", ");
            calls.push(parse_call(piece)?);
        }
        return (!calls.is_empty()).then_some(Op::Calls(calls));
    }
    let call = value.strip_prefix("await ")?;
    parse_call(call).map(|call| Op::Calls(vec![call]))
}

pub fn parse(script: &str) -> Result<Vec<Op>, String> {
    script
        .lines()
        .enumerate()
        .map(|(index, line)| {
            parse_line(line).ok_or_else(|| {
                format!(
                    "SyntaxError: the scripted engine cannot run line {}",
                    index + 1
                )
            })
        })
        .collect()
}

pub struct Plan<'a> {
    pub program: &'a Program,
    pub code: &'a Code,
}

impl Plan<'_> {
    fn nested_call(&self, rng: &mut Rng) -> (String, Json) {
        let tool = &self.program.tools[self.code.tools[rng.pick(self.code.tools.len())]];
        let mut input = BTreeMap::new();
        for param in &tool.params {
            let generated = crate::sim::generate(self.program, &param.ty, rng);
            input.insert(
                param.name.clone(),
                value::to_json(self.program, &param.ty, &generated),
            );
        }
        (tool.name.clone(), Json::Object(input))
    }

    pub fn ops(&self, rng: &mut Rng) -> Vec<Op> {
        let over = rng.chance(OVER_LIMIT_PPM);
        let calls_max = self.code.calls.value + u64::from(over);
        let timers_max = self.code.timers.value + u64::from(over);
        let length = rng.range(1, self.code.calls.value.max(1));
        let (mut calls, mut timers) = (0, 0);
        let mut ops = Vec::new();
        for _ in 0..length {
            let roll = rng.below(crate::sim::rng::PPM);
            let op = if roll < OP_CALL_PPM {
                calls += 1;
                Op::Calls(vec![self.nested_call(rng)])
            } else if roll < OP_CALL_PPM + OP_PARALLEL_PPM {
                let size = rng.range(2, self.code.parallel.value + 1);
                calls += size;
                Op::Calls((0..size).map(|_| self.nested_call(rng)).collect())
            } else if roll < OP_CALL_PPM + OP_PARALLEL_PPM + OP_SLEEP_PPM {
                timers += 1;
                let ms = if over {
                    self.code.timer_ms.value + 1
                } else {
                    rng.range(1, SLEEP_MS_MAX.min(self.code.timer_ms.value).max(1))
                };
                Op::Sleep(ms)
            } else if roll < OP_CALL_PPM + OP_PARALLEL_PPM + OP_SLEEP_PPM + OP_PRINT_PPM {
                Op::Print(crate::sim::world::SimWorld::generated_text(rng))
            } else {
                match rng.below(READ_KINDS) {
                    0 => Op::Clock,
                    1 => Op::Monotonic,
                    _ => Op::Random,
                }
            };
            if calls > calls_max || timers > timers_max {
                break;
            }
            ops.push(op);
        }
        if over {
            ops.push(Op::Calls(vec![self.nested_call(rng)]));
        }
        let roll = rng.below(crate::sim::rng::PPM);
        let text = crate::sim::world::SimWorld::generated_text(rng);
        if roll < END_RETURN_PPM {
            ops.push(Op::Return(text));
        } else if roll < END_RETURN_PPM + END_THROW_PPM {
            ops.push(Op::Throw(text));
        }
        ops
    }
}

#[derive(Clone, Debug)]
enum Await {
    Issuing {
        left: VecDeque<(String, Json)>,
        awaiting: BTreeSet<u64>,
    },
    Calls(BTreeSet<u64>),
    Arming(u64),
    Timer(u64),
    Value,
    Ended,
}

#[derive(Clone, Debug)]
pub struct Machine {
    ops: Vec<Op>,
    at: usize,
    next_call: u64,
    next_timer: u64,
    printed: Vec<String>,
    state: Await,
    steps: u64,
    pub diverge_at: Option<u64>,
    pub diverged: bool,
}

impl Machine {
    fn new(ops: Vec<Op>, diverge_at: Option<u64>) -> Self {
        Self {
            ops,
            at: 0,
            next_call: 0,
            next_timer: 0,
            printed: Vec::new(),
            state: Await::Value,
            steps: 0,
            diverge_at,
            diverged: false,
        }
    }

    fn output(&mut self, step: Step) -> Step {
        let index = self.steps;
        self.steps += 1;
        if self.diverge_at != Some(index) {
            return step;
        }
        self.diverged = true;
        match step {
            Step::Call { id, name, .. } => Step::Call {
                id,
                name,
                input: Json::object([("diverged", Json::Bool(true))]),
            },
            Step::Timeout { id, delay_ms } => Step::Timeout {
                id,
                delay_ms: delay_ms + 1,
            },
            Step::Clock => Step::Monotonic,
            Step::Monotonic | Step::Random => Step::Clock,
            Step::Bytes { length } => Step::Bytes { length: length + 1 },
            other => other,
        }
    }

    fn issue(&mut self, name: String, input: Json) -> (u64, Step) {
        self.next_call += 1;
        let id = self.next_call;
        (id, self.output(Step::Call { id, name, input }))
    }

    fn next(&mut self) -> Step {
        while let Some(op) = self.ops.get(self.at).cloned() {
            self.at += 1;
            let (state, step) = match op {
                Op::Calls(calls) => {
                    let mut left: VecDeque<(String, Json)> = calls.into_iter().collect();
                    let (name, input) = left.pop_front().expect("a calls op has a call");
                    let (id, step) = self.issue(name, input);
                    (
                        Await::Issuing {
                            left,
                            awaiting: BTreeSet::from([id]),
                        },
                        step,
                    )
                }
                Op::Sleep(delay_ms) => {
                    self.next_timer += 1;
                    let id = self.next_timer;
                    (
                        Await::Arming(id),
                        self.output(Step::Timeout { id, delay_ms }),
                    )
                }
                Op::Clock => (Await::Value, self.output(Step::Clock)),
                Op::Monotonic => (Await::Value, self.output(Step::Monotonic)),
                Op::Random => (Await::Value, self.output(Step::Random)),
                Op::Bytes(length) => (Await::Value, self.output(Step::Bytes { length })),
                Op::Print(text) => {
                    self.printed.push(text);
                    continue;
                }
                Op::Throw(error) => (Await::Ended, Step::Threw { error }),
                Op::Return(_) => (
                    Await::Ended,
                    Step::Done {
                        output: self.printed.join("\n"),
                    },
                ),
            };
            self.state = state;
            return step;
        }
        self.state = Await::Ended;
        Step::Done {
            output: self.printed.join("\n"),
        }
    }

    fn resume(&mut self, input: Input) -> Step {
        let state = std::mem::replace(&mut self.state, Await::Ended);
        match (state, input) {
            (
                Await::Issuing {
                    mut left,
                    mut awaiting,
                },
                Input::Issued,
            ) => match left.pop_front() {
                Some((name, input)) => {
                    let (id, step) = self.issue(name, input);
                    awaiting.insert(id);
                    self.state = Await::Issuing { left, awaiting };
                    step
                }
                None => {
                    self.state = Await::Calls(awaiting);
                    Step::Idle
                }
            },
            (Await::Calls(mut awaiting), Input::Result { id, .. }) => {
                awaiting.remove(&id);
                if awaiting.is_empty() {
                    return self.next();
                }
                self.state = Await::Calls(awaiting);
                Step::Idle
            }
            (Await::Arming(id), Input::Issued) => {
                self.state = Await::Timer(id);
                Step::Idle
            }
            (Await::Arming(_), Input::Refused { error }) => Step::Threw { error },
            (Await::Timer(id), Input::Timer { id: fired }) if id == fired => self.next(),
            (
                Await::Value,
                Input::Clock { .. }
                | Input::Monotonic { .. }
                | Input::Random { .. }
                | Input::Bytes { .. },
            ) => self.next(),
            (state, input) => Step::Threw {
                error: format!("the scripted engine got {input:?} while in {state:?}"),
            },
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SimEngine {
    pub machines: BTreeMap<u64, Machine>,
}

impl SimEngine {
    pub fn start_diverging(&mut self, cell: u64, script: &str, diverge_at: Option<u64>) -> Step {
        let ops = match parse(script) {
            Ok(ops) => ops,
            Err(error) => {
                self.machines.remove(&cell);
                return Step::Threw { error };
            }
        };
        let mut machine = Machine::new(ops, diverge_at);
        let step = machine.next();
        self.machines.insert(cell, machine);
        step
    }

    pub fn diverged(&self, cell: u64) -> bool {
        self.machines
            .get(&cell)
            .is_some_and(|machine| machine.diverged)
    }
}

impl CellEngine for SimEngine {
    fn start(&mut self, cell: u64, script: &str) -> Step {
        self.start_diverging(cell, script, None)
    }

    fn resume(&mut self, cell: u64, input: Input) -> Step {
        match self.machines.get_mut(&cell) {
            Some(machine) => machine.resume(input),
            None => Step::Threw {
                error: format!("the scripted engine has no cell {cell}"),
            },
        }
    }
}
