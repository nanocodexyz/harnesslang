pub mod faults;
pub mod invariants;
pub mod rng;
pub mod world;

use std::ops::Range;

use crate::ir::{Program, Type};
use crate::run::engine::{Exit, Finish};
use crate::run::journal::Record;
use crate::run::mutation::Mutation;
use crate::run::turn::{Ran, TurnRequest, run_turn};
use crate::run::value::Value;
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::rng::Rng;
use crate::sim::world::{RESTARTS_PER_TURN_MAX, SimWorld, Stats};
use crate::target::Target;

pub const TURNS_PER_SESSION_MAX: u64 = 4;
pub const RUNS_PER_TURN_MAX: u64 = RESTARTS_PER_TURN_MAX + 2;
pub const ARGUMENT_U64_MAX: u64 = u32::MAX as u64;

#[derive(Clone, Debug)]
pub struct Options {
    pub target: Target,
    pub seeds: Range<u64>,
    pub twice: bool,
    pub mutation: Mutation,
}

#[derive(Clone, Debug)]
pub struct SeedRun {
    pub journal: Vec<u8>,
    pub acks: Vec<Finish>,
    pub stats: Stats,
    pub profile: &'static str,
    pub result: Result<(), Violation>,
}

pub fn supported(program: &Program) -> Result<(), String> {
    for handler in &program.handlers {
        for local in &handler.params {
            let param = &handler.locals[*local];
            if contains_list(&param.ty) {
                return Err(format!("handler '{}': the simulator cannot generate the list argument '{}'", handler.name, param.name));
            }
        }
    }
    Ok(())
}

fn contains_list(ty: &Type) -> bool {
    match ty {
        Type::List { .. } => true,
        Type::Known(inner) | Type::Maybe(inner) => contains_list(inner),
        Type::U64 | Type::Bool | Type::Text | Type::Sum(_) => false,
    }
}

pub fn generate(program: &Program, ty: &Type, rng: &mut Rng) -> Value {
    match ty {
        Type::U64 => Value::U64(rng.below(ARGUMENT_U64_MAX)),
        Type::Bool => Value::Bool(rng.draw() & 1 == 1),
        Type::Text => Value::Text(SimWorld::generated_text(rng)),
        Type::List { .. } => panic!("supported() refuses list arguments"),
        Type::Sum(_) | Type::Known(_) | Type::Maybe(_) => {
            let variants = ty.variants(program).expect("a sum has variants");
            let variant = rng.pick(variants.len());
            let fields = variants[variant].fields.iter().map(|field| generate(program, field, rng)).collect();
            Value::Variant(variant, fields)
        }
    }
}

fn liveness(detail: String) -> Violation {
    Violation { invariant: Invariant::Liveness, detail }
}

fn drive_turn(program: &Program, world: &mut SimWorld<'_>, request: &TurnRequest) -> Result<(), Violation> {
    for _ in 0..RUNS_PER_TURN_MAX {
        let (records, epoch) = world.begin_run().map_err(|error| liveness(format!("turn {}: the journal cannot be read: {error}", request.turn)))?;
        let model = world.target;
        let mutation = world.mutation;
        match run_turn(program, &model, mutation, world, epoch, &records, request) {
            Ok(Ran::Finished(_)) => return Ok(()),
            Ok(Ran::AlreadyFinished(finish)) => {
                crate::run::World::acknowledge(world, &finish);
                return Ok(());
            }
            Err(Exit::Stop(_)) => {}
            Err(Exit::Corrupt(error)) => return Err(liveness(format!("turn {}: {error}", request.turn))),
        }
    }
    Err(liveness(format!("turn {} did not finish within runs_per_turn_max ({RUNS_PER_TURN_MAX})", request.turn)))
}

pub fn run_seed(program: &Program, target: Target, mutation: Mutation, seed: u64) -> SeedRun {
    let mut workload = Rng::derive(seed, "workload");
    let turns = workload.range(1, TURNS_PER_SESSION_MAX);
    let mut world = SimWorld::new(program, target, mutation, seed, true);
    let faulty = world.profile.crash_ppm > 0 || world.profile.lease_ppm > 0;
    let profile = world.profile.name;
    let mut result = Ok(());
    for turn in 1..=turns {
        let handler = &program.handlers[workload.pick(program.handlers.len())];
        let arguments = handler.params.iter().map(|local| generate(program, &handler.locals[*local].ty, &mut workload)).collect();
        let request = TurnRequest { turn, handler: handler.name.clone(), arguments };
        world.begin_turn(&request, faulty);
        if let Err(violation) = drive_turn(program, &mut world, &request) {
            let specific = match world.medium.durable() {
                Ok(records) => invariants::safety(&world, &records).err(),
                Err(_) => None,
            };
            result = Err(specific.unwrap_or(violation));
            break;
        }
    }
    let records = world.medium.durable();
    if result.is_ok() {
        result = match &records {
            Ok(records) => invariants::check(program, &world, turns, records),
            Err(error) => Err(liveness(format!("the final journal cannot be read: {error}"))),
        };
    }
    let mut stats = world.stats.clone();
    stats.unknown = records.map_or(0, |records| unknown_count(&records));
    SeedRun { journal: world.medium.bytes(), acks: world.acks.clone(), stats, profile, result }
}

fn unknown_count(records: &[Record]) -> u64 {
    records
        .iter()
        .map(|record| match record {
            Record::Recovery { unknown, .. } => unknown.len() as u64,
            _ => 0,
        })
        .sum()
}

#[derive(Clone, Debug)]
pub struct Failure {
    pub seed: u64,
    pub profile: &'static str,
    pub violation: Violation,
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub seeds: u64,
    pub stats: Stats,
    pub failure: Option<Failure>,
}

fn add(total: &mut Stats, more: &Stats) {
    total.turns += more.turns;
    total.runs += more.runs;
    total.crashes += more.crashes;
    total.power_losses += more.power_losses;
    total.torn_writes += more.torn_writes;
    total.fenced += more.fenced;
    total.takeovers += more.takeovers;
    total.dispatches += more.dispatches;
    total.unknown += more.unknown;
}

pub fn sweep(program: &Program, options: &Options) -> Summary {
    let mut summary = Summary::default();
    for seed in options.seeds.clone() {
        let first = run_seed(program, options.target, options.mutation, seed);
        summary.seeds += 1;
        add(&mut summary.stats, &first.stats);
        let mut result = first.result.clone();
        if result.is_ok() && options.twice {
            let second = run_seed(program, options.target, options.mutation, seed);
            if second.journal != first.journal || second.acks != first.acks {
                result = Err(Violation { invariant: Invariant::ReplayEqual, detail: format!("seed {seed} gave {} journal bytes then {}", first.journal.len(), second.journal.len()) });
            }
        }
        if let Err(violation) = result {
            summary.failure = Some(Failure { seed, profile: first.profile, violation });
            return summary;
        }
    }
    summary
}

pub fn report(summary: &Summary, options: &Options, file: &str) -> String {
    let stats = &summary.stats;
    match &summary.failure {
        Some(failure) => {
            let mut replay = format!("hl sim --target {} --seeds {}..{}", options.target.name(), failure.seed, failure.seed + 1);
            if options.twice {
                replay.push_str(" --twice");
            }
            if options.mutation != Mutation::None {
                replay.push_str(&format!(" --mutation {}", options.mutation.name()));
            }
            format!(
                "FAIL invariant={} seed={} target={} profile={} mutation={}: {}\nreplay: {replay} {file}",
                failure.violation.invariant.name(),
                failure.seed,
                options.target.name(),
                failure.profile,
                options.mutation.name(),
                failure.violation.detail
            )
        }
        None => format!(
            "ok target={} mutation={} seeds={}..{} ({} seeds{}): turns={} runs={} crashes={} power_losses={} torn_writes={} takeovers={} fenced={} dispatches={} unknown={}",
            options.target.name(),
            options.mutation.name(),
            options.seeds.start,
            options.seeds.end,
            summary.seeds,
            if options.twice { ", each twice" } else { "" },
            stats.turns,
            stats.runs,
            stats.crashes,
            stats.power_losses,
            stats.torn_writes,
            stats.takeovers,
            stats.fenced,
            stats.dispatches,
            stats.unknown
        ),
    }
}
