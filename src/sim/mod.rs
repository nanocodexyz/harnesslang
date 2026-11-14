pub mod cell;
pub mod dump;
pub mod events;
pub mod exhaustive;
pub mod faults;
pub mod invariants;
pub mod park;
pub mod rng;
pub mod strict;
pub mod trace;
pub mod world;

use std::ops::Range;

use crate::ir::{Program, Type};
use crate::run::engine::{Exit, Finish};
use crate::run::journal::Record;
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::turn::{Ran, TurnRequest, run_turn};
use crate::run::value::Value;
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::rng::Rng;
use crate::sim::strict::Edges;
use crate::sim::world::{RESTARTS_PER_TURN_MAX, SimWorld, Stats};
use crate::target::Target;

pub const TURNS_PER_SESSION_MAX: u64 = 4;
pub const RUNS_PER_TURN_MAX: u64 = RESTARTS_PER_TURN_MAX + 2;
pub const RUNS_PER_SUSPENSION: u64 = 2;
pub const ARGUMENT_U64_MAX: u64 = u32::MAX as u64;
pub const ARGUMENT_LIST_LENGTH_MAX: u64 = 4;

#[derive(Clone, Debug)]
pub struct Options {
    pub target: Target,
    pub seeds: Range<u64>,
    pub twice: bool,
    pub mutation: Mutation,
    pub strict: strict::Mode,
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
    for code in &program.codes {
        for tool in &code.tools {
            let tool = &program.tools[*tool];
            if let Some(param) = tool.params.iter().find(|param| contains_list(&param.ty)) {
                return Err(format!(
                    "code '{}': the simulator cannot generate the list input '{}' of nested tool '{}'",
                    code.name, param.name, tool.name
                ));
            }
        }
    }
    Ok(())
}

fn contains_list(ty: &Type) -> bool {
    match ty {
        Type::List { .. } => true,
        Type::Known(inner) | Type::Maybe(inner) => contains_list(inner),
        Type::U64 | Type::Bool | Type::Text | Type::Sum(_) | Type::Blob(_) => false,
    }
}

pub fn generate(program: &Program, ty: &Type, rng: &mut Rng, edges: Option<&Edges>) -> Value {
    match ty {
        Type::U64 => Value::U64(match edges {
            Some(edges) => edges.draw(rng),
            None => rng.below(ARGUMENT_U64_MAX),
        }),
        Type::Bool => Value::Bool(rng.draw() & 1 == 1),
        Type::Text => Value::Text(SimWorld::generated_text(rng)),
        Type::Blob(_) => panic!("the simulator draws no blob: blobs come from tools"),
        Type::List { element, .. } if element.holds_blob(program) => Value::List(Vec::new()),
        Type::List { element, capacity } => {
            let length = rng.below(capacity.value.min(ARGUMENT_LIST_LENGTH_MAX) + 1);
            Value::List(
                (0..length)
                    .map(|_| generate(program, element, rng, edges))
                    .collect(),
            )
        }
        Type::Sum(_) | Type::Known(_) | Type::Maybe(_) => {
            let variants = ty.variants(program).expect("a sum has variants");
            let variant = crate::sim::strict::blobless(program, &variants, rng);
            let fields = variants[variant]
                .fields
                .iter()
                .map(|field| generate(program, field, rng, edges))
                .collect();
            Value::Variant(variant, fields)
        }
    }
}

fn liveness(detail: String) -> Violation {
    Violation {
        invariant: Invariant::Liveness,
        detail,
    }
}

fn drive_turn(
    program: &Program,
    world: &mut SimWorld<'_>,
    request: &TurnRequest,
) -> Result<(), Violation> {
    let suspensions_max = program
        .handler(&request.handler)
        .expect("a turn request names a handler of the program")
        .worst
        .effects
        .total;
    let runs_max = RUNS_PER_TURN_MAX + RUNS_PER_SUSPENSION * suspensions_max;
    for _ in 0..runs_max {
        let (records, epoch) = world.begin_run().map_err(|error| {
            liveness(format!(
                "turn {}: the journal cannot be read: {error}",
                request.turn
            ))
        })?;
        let model = world.target;
        let mutation = world.mutation;
        let ran = run_turn(program, &model, mutation, world, epoch, &records, request);
        if let Some(trace) = &mut world.trace {
            trace.exit(&ran);
        }
        match ran {
            Ok(Ran::Finished(_)) => return Ok(()),
            Ok(Ran::Suspended(suspension)) => world.suspended(&suspension)?,
            Ok(Ran::AlreadyFinished(finish)) => {
                crate::run::World::acknowledge(world, &finish);
                return Ok(());
            }
            Err(Exit::Stop(_)) => {}
            Err(Exit::Corrupt(error)) => {
                return Err(liveness(format!("turn {}: {error}", request.turn)));
            }
        }
    }
    Err(liveness(format!(
        "turn {} did not finish within {runs_max} runs: runs_per_turn_max ({RUNS_PER_TURN_MAX}) and {RUNS_PER_SUSPENSION} per suspend its worst case allows",
        request.turn
    )))
}

pub fn run_seed(program: &Program, target: Target, mutation: Mutation, seed: u64) -> SeedRun {
    run_seed_traced(program, target, mutation, seed, None).0
}

pub fn run_seed_traced(
    program: &Program,
    target: Target,
    mutation: Mutation,
    seed: u64,
    trace: Option<trace::Trace>,
) -> (SeedRun, Option<trace::Trace>) {
    run_seed_checked(program, target, mutation, strict::Mode::Off, seed, trace)
}

pub fn run_seed_checked(
    program: &Program,
    target: Target,
    mutation: Mutation,
    strict: strict::Mode,
    seed: u64,
    trace: Option<trace::Trace>,
) -> (SeedRun, Option<trace::Trace>) {
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_seed_unguarded(program, target, mutation, strict, seed, trace)
    }));
    ran.unwrap_or_else(|payload| {
        let detail = payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic without a message".to_owned());
        let run = SeedRun {
            journal: Vec::new(),
            acks: Vec::new(),
            stats: Stats::default(),
            profile: faults::Profile::of(seed, target).name,
            result: Err(Violation {
                invariant: Invariant::Panic,
                detail,
            }),
        };
        (run, None)
    })
}

fn run_seed_unguarded(
    program: &Program,
    target: Target,
    mutation: Mutation,
    strict: strict::Mode,
    seed: u64,
    trace: Option<trace::Trace>,
) -> (SeedRun, Option<trace::Trace>) {
    let mut workload = Rng::derive(seed, "workload");
    let turns_max = if strict == strict::Mode::Off {
        TURNS_PER_SESSION_MAX
    } else {
        strict::TURNS_PER_SESSION_MAX
    };
    let turns = workload.range(1, turns_max);
    let mut world = SimWorld::new(program, target, mutation, seed, true);
    if strict != strict::Mode::Off {
        world.edges = Some(Edges::of(program));
        world.greedy = Rng::derive(seed, "persona").chance(strict::GREEDY_MODEL_PPM);
    }
    if strict == strict::Mode::FromAny {
        let edges = world.edges.clone().expect("set above");
        let mut rng = Rng::derive(seed, "entry");
        if let Some(state) = strict::entry(program, &mut rng, &edges) {
            world.seed_entry(&strict::entry_record(program, &state));
            world.stats.entry_states += 1;
        }
    }
    world.trace = trace;
    let profile_name = world.profile.name;
    world.record("seed", || {
        vec![
            ("seed", Json::Number(seed)),
            ("target", Json::text(target.name())),
            ("mutation", Json::text(mutation.name())),
            ("profile", Json::text(profile_name)),
        ]
    });
    let faulty = world.profile.crash_ppm > 0 || world.profile.lease_ppm > 0;
    let profile = world.profile.name;
    let mut result = Ok(());
    for turn in 1..=turns {
        let handler = &program.handlers[workload.pick(program.handlers.len())];
        let arguments = handler
            .params
            .iter()
            .map(|local| {
                let ty = &handler.locals[*local].ty;
                generate(program, ty, &mut workload, world.edges.as_ref())
            })
            .collect();
        let request = TurnRequest {
            turn,
            handler: handler.name.clone(),
            arguments,
        };
        world.begin_turn(&request, faulty);
        let driven = drive_turn(program, &mut world, &request);
        world.inbox.end_turn(turn);
        if let Err(violation) = driven {
            let specific = match world.medium.durable() {
                Ok(records) => invariants::safety(&world, &records)
                    .err()
                    .or_else(|| invariants::checkpoint_compacted(program, &world, &records).err()),
                Err(_) => None,
            };
            result = Err(specific.unwrap_or(violation));
            break;
        }
    }
    let records = world.medium.durable();
    if result.is_ok() {
        result = match &records {
            Ok(records) if strict != strict::Mode::Off => strict::overrun(&world.worst_faults)
                .and_then(|()| invariants::check(program, &world, turns, records))
                .and_then(|()| strict::check(program, &world.target, records)),
            Ok(records) => invariants::check(program, &world, turns, records),
            Err(error) => Err(liveness(format!(
                "the final journal cannot be read: {error}"
            ))),
        };
    }
    if result.is_ok()
        && profile == "clean"
        && program.has_parallel()
        && world.inbox.scheduled.is_empty()
        && let Ok(records) = &records
    {
        result = sequential_twin(
            program,
            target,
            mutation,
            strict,
            seed,
            records,
            &world.acks,
        );
    }
    let mut stats = world.stats.clone();
    stats.unknown = records.map_or(0, |records| unknown_count(&records));
    stats.events = program.events.any().then_some(world.inbox.recorded);
    let journal = world.medium.bytes();
    let acks = world.acks.len() as u64;
    let outcome = match &result {
        Ok(()) => "ok",
        Err(violation) => violation.invariant.name(),
    };
    world.record("end", || {
        vec![
            ("journal_bytes", Json::Number(journal.len() as u64)),
            (
                "journal_crc",
                Json::Number(u64::from(crate::run::crc::crc32(&journal))),
            ),
            ("acks", Json::Number(acks)),
            ("result", Json::text(outcome)),
        ]
    });
    let run = SeedRun {
        journal,
        acks: world.acks.clone(),
        stats,
        profile,
        result,
    };
    (run, world.trace.take())
}

fn sequential_twin(
    program: &Program,
    target: Target,
    mutation: Mutation,
    strict: strict::Mode,
    seed: u64,
    records: &[Record],
    acks: &[Finish],
) -> Result<(), Violation> {
    let twin = program.sequential();
    assert!(
        !twin.has_parallel(),
        "the sequential twin runs one call at a time"
    );
    let (run, _) = run_seed_unguarded(&twin, target, mutation, strict, seed, None);
    if invariants::trapped_in_body(&run.acks) {
        return Ok(());
    }
    let checksum = target == Target::NativeJsonl && mutation != Mutation::JsonlNoChecksum;
    let sequential =
        crate::run::journal::decode_lines(&run.journal, checksum).map_err(|error| {
            liveness(format!(
                "the sequential twin's journal cannot be read: {error}"
            ))
        })?;
    invariants::sequential_equal(records, acks, &sequential.records, &run.acks)
}

fn unknown_count(records: &[Record]) -> u64 {
    records
        .iter()
        .map(|record| match record {
            Record::Recovery {
                unknown, nested, ..
            } => (unknown.len() + nested.len()) as u64,
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
    total.cells += more.cells;
    total.nested += more.nested;
    total.divergences += more.divergences;
    total.waits += more.waits;
    total.sleeps += more.sleeps;
    total.suspensions += more.suspensions;
    total.events = match (total.events, more.events) {
        (None, None) => None,
        (left, right) => Some(left.unwrap_or(0) + right.unwrap_or(0)),
    };
    total.entry_states += more.entry_states;
}

pub fn sweep(program: &Program, options: &Options) -> Summary {
    sweep_traced(program, options, None).expect("a sweep without a trace writes nothing")
}

pub fn sweep_traced(
    program: &Program,
    options: &Options,
    mut sink: Option<&mut dyn std::io::Write>,
) -> Result<Summary, String> {
    let mut summary = Summary::default();
    for seed in options.seeds.clone() {
        let trace = sink.as_ref().map(|_| trace::Trace::default());
        let (first, trace) = run_seed_checked(
            program,
            options.target,
            options.mutation,
            options.strict,
            seed,
            trace,
        );
        if let (Some(sink), Some(trace)) = (sink.as_mut(), trace) {
            for line in &trace.lines {
                writeln!(sink, "{line}")
                    .map_err(|error| format!("trace: cannot write: {error}"))?;
            }
        }
        summary.seeds += 1;
        add(&mut summary.stats, &first.stats);
        let mut result = first.result.clone();
        if result.is_ok() && options.twice {
            let (second, _) = run_seed_checked(
                program,
                options.target,
                options.mutation,
                options.strict,
                seed,
                None,
            );
            if second.journal != first.journal || second.acks != first.acks {
                let offset = first
                    .journal
                    .iter()
                    .zip(&second.journal)
                    .position(|(left, right)| left != right)
                    .unwrap_or(first.journal.len().min(second.journal.len()));
                result = Err(Violation {
                    invariant: Invariant::ReplayEqual,
                    detail: format!(
                        "two runs of seed {seed} wrote journals of {} and {} bytes that first differ at byte {offset}",
                        first.journal.len(),
                        second.journal.len()
                    ),
                });
            }
        }
        if let Err(violation) = result {
            summary.failure = Some(Failure {
                seed,
                profile: first.profile,
                violation,
            });
            return Ok(summary);
        }
    }
    Ok(summary)
}

pub fn report(summary: &Summary, options: &Options, file: &str) -> String {
    let stats = &summary.stats;
    match &summary.failure {
        Some(failure) => {
            let mut replay = format!(
                "hl sim --target {} --seeds {}..{}",
                options.target.name(),
                failure.seed,
                failure.seed + 1
            );
            if options.twice {
                replay.push_str(" --twice");
            }
            match options.strict {
                strict::Mode::Off => {}
                strict::Mode::FromZero => replay.push_str(" --strict"),
                strict::Mode::FromAny => replay.push_str(" --strict --any-state"),
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
            "ok target={} mutation={} seeds={}..{} ({} seeds{}{}): turns={} runs={} crashes={} power_losses={} torn_writes={} takeovers={} fenced={} dispatches={} unknown={}{}",
            options.target.name(),
            options.mutation.name(),
            options.seeds.start,
            options.seeds.end,
            summary.seeds,
            if options.twice { ", each twice" } else { "" },
            match options.strict {
                strict::Mode::Off => String::new(),
                strict::Mode::FromZero => ", strict".to_owned(),
                strict::Mode::FromAny => format!(
                    ", strict, {} from an arbitrary entry state",
                    stats.entry_states
                ),
            },
            stats.turns,
            stats.runs,
            stats.crashes,
            stats.power_losses,
            stats.torn_writes,
            stats.takeovers,
            stats.fenced,
            stats.dispatches,
            stats.unknown,
            format!(
                "{}{}{}",
                if stats.cells > 0 {
                    format!(
                        " cells={} nested={} divergences={} waits={}",
                        stats.cells, stats.nested, stats.divergences, stats.waits
                    )
                } else {
                    String::new()
                },
                stats
                    .events
                    .map_or_else(String::new, |events| format!(" events={events}")),
                if stats.sleeps + stats.suspensions > 0 {
                    format!(" sleeps={} suspensions={}", stats.sleeps, stats.suspensions)
                } else {
                    String::new()
                }
            ),
        ),
    }
}
