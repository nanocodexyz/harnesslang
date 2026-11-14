use std::collections::BTreeMap;

use crate::effect::EffectClass;
use crate::ir::{self, Program, Type};
use crate::run::engine::{Finish, ModelRequest, Point, Stop, ToolRequest, World};
use crate::run::journal::{self, Output, Record};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::turn::{Ran, TurnRequest, run_turn};
use crate::run::value;
use crate::sim::faults::{PowerLoss, Profile, SECTOR_PERSISTED_PPM};
use crate::sim::rng::Rng;
use crate::sim::strict::{self, Edges};
use crate::sim::trace::{self, Trace};
use crate::target::medium::Medium;
use crate::target::{self, Target};

pub const RESTARTS_PER_TURN_MAX: u64 = 12;
pub const ANSWER_PPM: u64 = 350_000;
pub const UNDECLARED_TOOL_PPM: u64 = 250_000;
pub const OVER_CAPACITY_PPM: u64 = 5_000;
pub const TOOL_FAILURE_PPM: u64 = 150_000;
pub const MODEL_FAILURE_PPM: u64 = 150_000;
pub const TEXT_ENUM_INVALID_PPM: u64 = 100_000;
pub const CLOCK_STEP_MAX: u64 = 1_000;
pub const UNDECLARED_TOOLS: [&str; 2] = ["web_fetch", "spawn_agent"];
pub const WORDS: [&str; 8] = [
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dispatched {
    pub tool: String,
    pub external: u64,
    pub other: u64,
    pub outputs: Vec<Output>,
    pub values: Vec<Returned>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Returned {
    pub tool: String,
    pub input: Json,
    pub output: Output,
    pub expected: Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinishFlush {
    pub turn: u64,
    pub checkpointed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub turns: u64,
    pub runs: u64,
    pub crashes: u64,
    pub power_losses: u64,
    pub torn_writes: u64,
    pub fenced: u64,
    pub takeovers: u64,
    pub dispatches: u64,
    pub unknown: u64,
    pub entry_states: u64,
}

#[derive(Clone)]
pub struct SimWorld<'a> {
    pub program: &'a Program,
    pub target: target::Model,
    pub mutation: Mutation,
    pub seed: u64,
    pub profile: Profile,
    pub medium: Medium,
    pub dispatched: BTreeMap<String, Dispatched>,
    pub acks: Vec<Finish>,
    pub ack_faults: Vec<String>,
    pub owner_faults: Vec<String>,
    pub finish_flushes: Vec<FinishFlush>,
    pub stats: Stats,
    pub trace: Option<Trace>,
    pub edges: Option<Edges>,
    pub greedy: bool,
    pub worst_faults: Vec<String>,
    run_effects: u64,
    run_outbound: u64,
    rng: Rng,
    faults_left: u64,
    armed: bool,
    nested: bool,
    lease_spent: bool,
    current: Option<TurnRequest>,
    clock: u64,
    model_calls: BTreeMap<(u64, u64), u64>,
}

impl<'a> SimWorld<'a> {
    pub fn new(
        program: &'a Program,
        target: Target,
        mutation: Mutation,
        seed: u64,
        faults: bool,
    ) -> Self {
        let profile = Profile::of(seed, target);
        Self {
            program,
            target: target::model(target),
            mutation,
            seed,
            profile,
            medium: Medium::new(target, mutation),
            dispatched: BTreeMap::new(),
            acks: Vec::new(),
            ack_faults: Vec::new(),
            owner_faults: Vec::new(),
            finish_flushes: Vec::new(),
            stats: Stats::default(),
            trace: None,
            edges: None,
            greedy: false,
            worst_faults: Vec::new(),
            run_effects: 0,
            run_outbound: 0,
            rng: Rng::derive(seed, "faults"),
            faults_left: 0,
            armed: faults,
            nested: false,
            lease_spent: false,
            current: None,
            clock: 0,
            model_calls: BTreeMap::new(),
        }
    }

    pub fn record(&mut self, name: &str, fields: impl FnOnce() -> Vec<(&'static str, Json)>) {
        if let Some(trace) = &mut self.trace {
            trace.event(name, fields());
        }
    }

    fn opened(&mut self, name: &str, epoch: u64, bytes: Option<Vec<u8>>) {
        if let (Some(trace), Some(bytes)) = (&mut self.trace, bytes) {
            trace.opened(name, epoch, &bytes);
        }
    }

    fn traced_bytes(&self) -> Option<Vec<u8>> {
        self.trace.as_ref().map(|_| self.medium.bytes())
    }

    pub fn begin_turn(&mut self, request: &TurnRequest, faults: bool) {
        let program = self.program;
        self.record("turn", || {
            let handler = program
                .handler(&request.handler)
                .expect("a turn request names a handler of the program");
            vec![
                ("turn", Json::Number(request.turn)),
                ("handler", Json::text(&request.handler)),
                (
                    "arguments",
                    crate::run::turn::arguments_json(program, handler, &request.arguments),
                ),
            ]
        });
        self.current = Some(request.clone());
        self.faults_left = if faults { RESTARTS_PER_TURN_MAX } else { 0 };
        self.armed = faults;
        self.lease_spent = false;
        self.stats.turns += 1;
    }

    pub fn seed_entry(&mut self, record: &Record) {
        match &mut self.medium {
            Medium::Jsonl(file) => {
                let bytes = journal::encode_lines(std::slice::from_ref(record), file.checksum);
                file.write(&bytes);
                file.sync();
            }
            Medium::Rows(rows) => rows.commit(std::slice::from_ref(record)),
        }
    }

    pub fn begin_run(&mut self) -> Result<(Vec<Record>, u64), String> {
        self.stats.runs += 1;
        self.run_effects = 0;
        self.run_outbound = 0;
        if self.faults_left == 0 {
            self.armed = false;
        }
        let bytes = self.traced_bytes();
        let records = match self.medium.open() {
            Ok(records) => records,
            Err(error) => {
                self.opened("run", 0, bytes);
                return Err(error);
            }
        };
        let epoch = match &mut self.medium {
            Medium::Rows(rows) if self.target.target == Target::ServerPostgres => rows.take_over(),
            Medium::Rows(rows) => rows.epoch,
            Medium::Jsonl(_) => 0,
        };
        self.opened("run", epoch, bytes);
        Ok((records, epoch))
    }

    fn crash(&mut self) -> Stop {
        assert!(
            self.faults_left > 0,
            "a crash spends the turn's fault budget"
        );
        self.faults_left -= 1;
        self.stats.crashes += 1;
        let power_loss = self.profile.power_loss;
        let Medium::Jsonl(file) = &mut self.medium else {
            return Stop::Crash;
        };
        if file.unsynced() == 0 {
            return Stop::Crash;
        }
        match power_loss {
            PowerLoss::Never => {}
            PowerLoss::Prefix { ppm } => {
                if self.rng.chance(ppm) {
                    let kept = self.rng.below(file.unsynced() as u64 + 1) as usize;
                    file.lose_unsynced_prefix(kept);
                    self.stats.power_losses += 1;
                }
            }
            PowerLoss::Sectors { ppm } => {
                if self.rng.chance(ppm) {
                    let persisted: Vec<bool> = (0..file.unsynced_sectors())
                        .map(|_| self.rng.chance(SECTOR_PERSISTED_PPM))
                        .collect();
                    file.lose_unsynced_sectors(&persisted);
                    self.stats.power_losses += 1;
                }
            }
        }
        Stop::Crash
    }

    fn fault(&mut self, ppm: u64) -> bool {
        !self.nested && self.armed && self.faults_left > 0 && self.rng.chance(ppm)
    }

    fn second_owner(&mut self) {
        self.lease_spent = true;
        self.stats.takeovers += 1;
        let request = self.current.clone().expect("a lease expires during a turn");
        let program = self.program;
        let model = self.target;
        let mutation = self.mutation;
        let Medium::Rows(rows) = &mut self.medium else {
            panic!("only the postgres target has leases");
        };
        let epoch = rows.take_over();
        let bytes = self.traced_bytes();
        self.opened("owner", epoch, bytes);
        let records = match self.medium.open() {
            Ok(records) => records,
            Err(error) => {
                self.owner_faults
                    .push(format!("second owner could not read the journal: {error}"));
                return;
            }
        };
        let counted = (self.run_effects, self.run_outbound);
        (self.run_effects, self.run_outbound) = (0, 0);
        self.nested = true;
        let ran = run_turn(program, &model, mutation, self, epoch, &records, &request);
        self.nested = false;
        (self.run_effects, self.run_outbound) = counted;
        if let Some(trace) = &mut self.trace {
            trace.exit(&ran);
        }
        match ran {
            Ok(Ran::Finished(_)) => {}
            Ok(Ran::AlreadyFinished(finish)) => self.acknowledge(&finish),
            Err(exit) => self
                .owner_faults
                .push(format!("second owner stopped: {exit:?}")),
        }
    }

    pub fn generated_text(rng: &mut Rng) -> String {
        format!(
            "{}-{}",
            WORDS[rng.pick(WORDS.len())],
            rng.below(WORDS.len() as u64 * WORDS.len() as u64)
        )
    }

    fn tool_value(program: &Program, rng: &mut Rng, returns: &Type, edges: Option<&Edges>) -> Json {
        match returns {
            Type::U64 => Json::Number(match edges {
                Some(edges) => edges.draw(rng),
                None => rng.draw() >> 32,
            }),
            Type::Bool => Json::Bool(rng.draw() & 1 == 1),
            Type::Text => Json::String(Self::generated_text(rng)),
            Type::Sum(_) => {
                let sum = program
                    .text_enum(returns)
                    .expect("the checker admits only text enums among sum tool results");
                if rng.chance(TEXT_ENUM_INVALID_PPM) {
                    return Json::String(Self::generated_text(rng));
                }
                Json::String(sum.variants[rng.pick(sum.variants.len())].name.clone())
            }
            other => panic!("the checker admits only scalar tool results, found {other:?}"),
        }
    }

    fn count_effect(&mut self, outbound: bool) {
        if self.edges.is_none() {
            return;
        }
        let request = self.current.as_ref().expect("an effect runs within a turn");
        let worst = &self
            .program
            .handler(&request.handler)
            .expect("a turn request names a handler of the program")
            .worst;
        self.run_effects += 1;
        let mut faults = Vec::new();
        if self.run_effects == worst.effects.total + 1 {
            faults.push(format!(
                "{} effects, over the worst case of {}",
                self.run_effects, worst.effects.total
            ));
        }
        if outbound {
            self.run_outbound += 1;
            if self.run_outbound == worst.requests.total + 1 {
                faults.push(format!(
                    "{} outbound requests, over the worst case of {}",
                    self.run_outbound, worst.requests.total
                ));
            }
            if let Some(limit) = &self.target.limits.outbound_requests_per_invocation
                && self.run_outbound == limit.value + 1
            {
                faults.push(format!(
                    "{} outbound requests, over the limit of {} {}",
                    self.run_outbound, limit.value, limit.unit
                ));
            }
        }
        for fault in faults {
            self.worst_faults.push(format!(
                "turn {} ({}): one run made {fault}",
                request.turn, request.handler
            ));
        }
    }

    fn durable_status(&self, turn: u64) -> Option<Record> {
        let records = self.medium.durable().ok()?;
        records.into_iter().find(|record| matches!(record, Record::Status { turn: status_turn, .. } if *status_turn == turn))
    }
}

impl World for SimWorld<'_> {
    fn hook(&mut self, point: Point) -> Result<(), Stop> {
        let record = |answer: Json| {
            move || {
                let mut fields = trace::point_fields(point);
                fields.push(("answer", answer));
                fields
            }
        };
        if self.nested {
            self.record("hook", record(Json::text("ok")));
            return Ok(());
        }
        let lease = self.target.target == Target::ServerPostgres && !self.lease_spent;
        if lease && self.armed && self.rng.chance(self.profile.lease_ppm) {
            self.record("hook", record(Json::text("owner")));
            self.second_owner();
            return Ok(());
        }
        let result = if self.fault(self.profile.crash_ppm) {
            Err(self.crash())
        } else {
            Ok(())
        };
        self.record("hook", record(trace::answer(&result)));
        result
    }

    fn flush(&mut self, epoch: u64, records: &[Record]) -> Result<(), Stop> {
        for record in records {
            if let Record::Status { turn, .. } = record {
                let checkpointed = records.iter().any(|other| {
                    matches!(other, Record::Checkpoint { revision, .. } if revision == turn)
                });
                self.finish_flushes.push(FinishFlush {
                    turn: *turn,
                    checkpointed,
                });
            }
        }
        let result = self.flush_medium(epoch, records);
        let checksum = match &self.medium {
            Medium::Jsonl(file) => file.checksum,
            Medium::Rows(_) => false,
        };
        self.record("flush", || {
            let bytes = journal::encode_lines(records, checksum);
            vec![
                ("epoch", Json::Number(epoch)),
                (
                    "bytes",
                    Json::String(String::from_utf8(bytes).expect("encoded lines are utf-8")),
                ),
                ("answer", trace::answer(&result)),
            ]
        });
        result
    }

    fn attempt(&mut self, turn: u64, seq: u64) -> Result<u64, Stop> {
        let attempt = self.count_attempt(turn, seq)?;
        self.record("attempt", || {
            vec![
                ("turn", Json::Number(turn)),
                ("seq", Json::Number(seq)),
                ("answer", Json::Number(attempt)),
            ]
        });
        Ok(attempt)
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        self.count_effect(true);
        let output = self.tool_output(request);
        self.record("tool", || {
            vec![
                ("turn", Json::Number(request.turn)),
                ("seq", Json::Number(request.seq)),
                ("tool", Json::text(request.tool)),
                ("effect", Json::text(request.effect.name())),
                ("input", request.input.clone()),
                ("output", journal::output_json(&output)),
            ]
        });
        output
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        self.count_effect(true);
        let response = self.reply(request);
        let program = self.program;
        self.record("model", || {
            let context = Json::Array(
                request
                    .context
                    .as_list()
                    .iter()
                    .map(|entry| value::to_json(program, &Type::Sum(ir::ENTRY), entry))
                    .collect(),
            );
            vec![
                ("turn", Json::Number(request.turn)),
                ("seq", Json::Number(request.seq)),
                ("calls_max", Json::Number(request.calls_max)),
                (
                    "context_crc",
                    Json::Number(u64::from(crate::run::crc::crc32(
                        context.encode().as_bytes(),
                    ))),
                ),
                ("response", response.clone()),
            ]
        });
        response
    }

    fn now(&mut self) -> u64 {
        self.count_effect(false);
        let clock = match &self.edges {
            Some(edges) => edges.draw(&mut self.rng),
            None => {
                self.clock += self.rng.range(1, CLOCK_STEP_MAX);
                self.clock
            }
        };
        self.record("now", || vec![("answer", Json::Number(clock))]);
        clock
    }

    fn random(&mut self) -> u64 {
        self.count_effect(false);
        let random = match &self.edges {
            Some(edges) => edges.draw(&mut self.rng),
            None => self.rng.draw(),
        };
        self.record("random", || vec![("answer", Json::Number(random))]);
        random
    }

    fn acknowledge(&mut self, finish: &Finish) {
        self.record("ack", || {
            vec![
                ("turn", Json::Number(finish.turn)),
                ("outcome", Json::text(finish.outcome.name())),
                ("result", finish.result.clone()),
            ]
        });
        self.check_acknowledge(finish);
    }
}

impl SimWorld<'_> {
    fn flush_medium(&mut self, epoch: u64, records: &[Record]) -> Result<(), Stop> {
        let fence = self.mutation != Mutation::PostgresNoFence
            && self.target.target == Target::ServerPostgres;
        if let Medium::Rows(rows) = &mut self.medium {
            if fence && rows.fenced(epoch) {
                self.stats.fenced += 1;
                return Err(Stop::Fenced);
            }
            rows.commit(records);
            return Ok(());
        }
        let torn = self.fault(self.profile.torn_ppm);
        let unsynced_crash = !torn && self.fault(self.profile.unsynced_crash_ppm);
        let Medium::Jsonl(file) = &mut self.medium else {
            unreachable!("rows returned above");
        };
        let bytes = journal::encode_lines(records, file.checksum);
        if torn {
            let cut = self.rng.below(bytes.len() as u64) as usize;
            file.write(&bytes[..cut]);
            self.stats.torn_writes += 1;
            return Err(self.crash());
        }
        file.write(&bytes);
        if unsynced_crash {
            return Err(self.crash());
        }
        file.sync();
        Ok(())
    }

    fn count_attempt(&mut self, turn: u64, seq: u64) -> Result<u64, Stop> {
        match &mut self.medium {
            Medium::Rows(rows) => Ok(rows.attempt(turn, seq)),
            Medium::Jsonl(_) => panic!("only step targets count attempts"),
        }
    }

    fn tool_output(&mut self, request: &ToolRequest<'_>) -> Output {
        let call = journal::call_id(request.turn, request.seq);
        let entry = self.dispatched.entry(call).or_default();
        entry.tool = request.tool.to_owned();
        if request.effect == EffectClass::External {
            entry.external += 1;
        } else {
            entry.other += 1;
        }
        let count = entry.external + entry.other;
        self.stats.dispatches += 1;
        let mut rng = Rng::derive_keyed(self.seed, "tool", &[request.turn, request.seq, count]);
        let output = if rng.chance(TOOL_FAILURE_PPM) {
            Output::Failed(format!(
                "{} failed: {}",
                request.tool,
                Self::generated_text(&mut rng)
            ))
        } else {
            Output::Ok(Self::tool_value(
                self.program,
                &mut rng,
                request.returns,
                self.edges.as_ref(),
            ))
        };
        let expected =
            value::checked_output(self.program, request.tool, request.returns, output.clone());
        let checked = self.program.text_enum(request.returns).is_some();
        let entry = self
            .dispatched
            .get_mut(&journal::call_id(request.turn, request.seq))
            .expect("inserted above");
        if request.effect == EffectClass::External {
            entry.outputs.push(expected.clone());
        }
        if checked {
            entry.values.push(Returned {
                tool: request.tool.to_owned(),
                input: request.input.clone(),
                output: output.clone(),
                expected,
            });
        }
        output
    }

    fn reply(&mut self, request: &ModelRequest<'_>) -> Json {
        let count = self
            .model_calls
            .entry((request.turn, request.seq))
            .or_insert(0);
        *count += 1;
        let mut rng = Rng::derive_keyed(self.seed, "model", &[request.turn, request.seq, *count]);
        let model = self
            .program
            .model
            .as_ref()
            .expect("the checker admits model calls only with a model");
        if model.declared() && !self.greedy && rng.chance(MODEL_FAILURE_PPM) {
            let error = format!("{} failed: {}", model.name, Self::generated_text(&mut rng));
            return Json::object([(ir::MODEL_FAILED, Json::String(error))]);
        }
        let (calls_variant, calls_field) = model.calls_at();
        let variants = &self.program.sums[ir::REPLY].variants;
        let others: Vec<usize> = (0..variants.len())
            .filter(|variant| *variant != calls_variant)
            .collect();
        if !others.is_empty() && !self.greedy && rng.chance(ANSWER_PPM) {
            let variant = match others.as_slice() {
                [only] => *only,
                _ => others[rng.pick(others.len())],
            };
            let fields = variants[variant]
                .fields
                .iter()
                .map(|field| Self::tool_value(self.program, &mut rng, field, self.edges.as_ref()))
                .collect();
            return Self::variant_json(&variants[variant].name, fields);
        }
        let calls = self.drawn_calls(&mut rng, request);
        let definition = &self.program.sums[ir::REPLY].variants[calls_variant];
        let mut calls = Some(calls);
        let fields = (0..definition.fields.len())
            .map(|field| {
                if field == calls_field {
                    calls.take().expect("one field carries the calls")
                } else {
                    Self::tool_value(
                        self.program,
                        &mut rng,
                        &definition.fields[field],
                        self.edges.as_ref(),
                    )
                }
            })
            .collect();
        Self::variant_json(&definition.name, fields)
    }

    fn variant_json(name: &str, mut fields: Vec<Json>) -> Json {
        match fields.len() {
            0 => Json::String(name.to_owned()),
            1 => Json::object([(name, fields.pop().expect("one field"))]),
            _ => Json::object([(name, Json::Array(fields))]),
        }
    }

    fn drawn_calls(&self, rng: &mut Rng, request: &ModelRequest<'_>) -> Json {
        let over_ppm = match (self.greedy, self.edges.is_some()) {
            (true, _) => 0,
            (false, true) => strict::OVER_CAPACITY_PPM,
            (false, false) => OVER_CAPACITY_PPM,
        };
        let over = rng.chance(over_ppm);
        let count = if over {
            request.calls_max + 1
        } else if self.greedy {
            request.calls_max
        } else {
            rng.range(1, request.calls_max.max(1))
        };
        let mut calls = Vec::new();
        for index in 0..count {
            let tool = if self.program.tools.is_empty() || rng.chance(UNDECLARED_TOOL_PPM) {
                UNDECLARED_TOOLS[rng.pick(UNDECLARED_TOOLS.len())].to_owned()
            } else {
                self.program.tools[rng.pick(self.program.tools.len())]
                    .name
                    .clone()
            };
            let mut fields = BTreeMap::new();
            fields.insert(
                ir::CALL_FIELDS[ir::CALL_ID].to_owned(),
                Json::String(format!("c{}-{}-{index}", request.turn, request.seq)),
            );
            fields.insert(
                ir::CALL_FIELDS[ir::CALL_TOOL].to_owned(),
                Json::String(tool),
            );
            fields.insert(
                ir::CALL_FIELDS[ir::CALL_INPUT].to_owned(),
                Json::String(Self::generated_text(rng)),
            );
            calls.push(Json::Object(fields));
        }
        Json::Array(calls)
    }

    fn check_acknowledge(&mut self, finish: &Finish) {
        let durable = self.durable_status(finish.turn);
        let agrees = matches!(&durable, Some(Record::Status { outcome, result, .. }) if *outcome == finish.outcome && *result == finish.result);
        if !agrees {
            self.ack_faults.push(format!(
                "turn {} was acknowledged {} {} while its durable status was {:?}",
                finish.turn,
                finish.outcome.name(),
                finish.result.encode(),
                durable.map(|record| journal::to_json(&record).encode())
            ));
        }
        self.acks.push(finish.clone());
    }
}
