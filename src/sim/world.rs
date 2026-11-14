use std::collections::BTreeMap;

use crate::effect::EffectClass;
use crate::ir::{self, Program, Type};
use crate::run::engine::{Finish, ModelRequest, Point, Stop, ToolRequest, World};
use crate::run::journal::{self, Output, Record};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::turn::{Ran, TurnRequest, run_turn};
use crate::sim::faults::{PowerLoss, Profile, SECTOR_PERSISTED_PPM};
use crate::sim::rng::Rng;
use crate::target::medium::Medium;
use crate::target::{self, Target};

pub const RESTARTS_PER_TURN_MAX: u64 = 12;
pub const ANSWER_PPM: u64 = 350_000;
pub const UNDECLARED_TOOL_PPM: u64 = 250_000;
pub const OVER_CAPACITY_PPM: u64 = 5_000;
pub const TOOL_FAILURE_PPM: u64 = 150_000;
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
}

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
    pub stats: Stats,
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
            stats: Stats::default(),
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

    pub fn begin_turn(&mut self, request: &TurnRequest, faults: bool) {
        self.current = Some(request.clone());
        self.faults_left = if faults { RESTARTS_PER_TURN_MAX } else { 0 };
        self.armed = faults;
        self.lease_spent = false;
        self.stats.turns += 1;
    }

    pub fn begin_run(&mut self) -> Result<(Vec<Record>, u64), String> {
        self.stats.runs += 1;
        if self.faults_left == 0 {
            self.armed = false;
        }
        let records = self.medium.open()?;
        let epoch = match &mut self.medium {
            Medium::Rows(rows) if self.target.target == Target::ServerPostgres => rows.take_over(),
            Medium::Rows(rows) => rows.epoch,
            Medium::Jsonl(_) => 0,
        };
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
        let records = match self.medium.open() {
            Ok(records) => records,
            Err(error) => {
                self.owner_faults
                    .push(format!("second owner could not read the journal: {error}"));
                return;
            }
        };
        self.nested = true;
        let ran = run_turn(program, &model, mutation, self, epoch, &records, &request);
        self.nested = false;
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

    fn tool_value(rng: &mut Rng, returns: &Type) -> Json {
        match returns {
            Type::U64 => Json::Number(rng.draw() >> 32),
            Type::Bool => Json::Bool(rng.draw() & 1 == 1),
            Type::Text => Json::String(Self::generated_text(rng)),
            other => panic!("the checker admits only scalar tool results, found {other:?}"),
        }
    }

    fn durable_status(&self, turn: u64) -> Option<Record> {
        let records = self.medium.durable().ok()?;
        records.into_iter().find(|record| matches!(record, Record::Status { turn: status_turn, .. } if *status_turn == turn))
    }
}

impl World for SimWorld<'_> {
    fn hook(&mut self, _point: Point) -> Result<(), Stop> {
        if self.nested {
            return Ok(());
        }
        let lease = self.target.target == Target::ServerPostgres && !self.lease_spent;
        if lease && self.armed && self.rng.chance(self.profile.lease_ppm) {
            self.second_owner();
            return Ok(());
        }
        if self.fault(self.profile.crash_ppm) {
            return Err(self.crash());
        }
        Ok(())
    }

    fn flush(&mut self, epoch: u64, records: &[Record]) -> Result<(), Stop> {
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

    fn attempt(&mut self, turn: u64, seq: u64) -> Result<u64, Stop> {
        match &mut self.medium {
            Medium::Rows(rows) => Ok(rows.attempt(turn, seq)),
            Medium::Jsonl(_) => panic!("only step targets count attempts"),
        }
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
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
            Output::Ok(Self::tool_value(&mut rng, request.returns))
        };
        let entry = self
            .dispatched
            .get_mut(&journal::call_id(request.turn, request.seq))
            .expect("inserted above");
        if request.effect == EffectClass::External {
            entry.outputs.push(output.clone());
        }
        output
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        let count = self
            .model_calls
            .entry((request.turn, request.seq))
            .or_insert(0);
        *count += 1;
        let mut rng = Rng::derive_keyed(self.seed, "model", &[request.turn, request.seq, *count]);
        if rng.chance(ANSWER_PPM) {
            return Json::object([("answer", Json::String(Self::generated_text(&mut rng)))]);
        }
        let over = rng.chance(OVER_CAPACITY_PPM);
        let count = if over {
            request.calls_max + 1
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
                Json::String(Self::generated_text(&mut rng)),
            );
            calls.push(Json::Object(fields));
        }
        Json::object([("calls", Json::Array(calls))])
    }

    fn now(&mut self) -> u64 {
        self.clock += self.rng.range(1, CLOCK_STEP_MAX);
        self.clock
    }

    fn random(&mut self) -> u64 {
        self.rng.draw()
    }

    fn acknowledge(&mut self, finish: &Finish) {
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
