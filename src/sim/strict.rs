use std::collections::BTreeMap;

use crate::ir::{Program, Type};
use crate::run::engine;
use crate::run::journal::{Outcome, Record};
use crate::run::json::Json;
use crate::run::session;
use crate::run::turn;
use crate::run::value::{self, Value};
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::rng::Rng;
use crate::sim::world::SimWorld;
use crate::target;

pub const TURNS_PER_SESSION_MAX: u64 = 12;
pub const OVER_CAPACITY_PPM: u64 = 50_000;
pub const EDGE_PPM: u64 = 500_000;
pub const MODEL_REPLY_TRAP: &str = "model reply: ";
pub const ALLOWED_TRAPS: [&str; 2] = ["requires(", MODEL_REPLY_TRAP];
pub const ENTRY_DRAWS_MAX: u64 = 64;
pub const GREEDY_MODEL_PPM: u64 = 250_000;
const U64_BITS: u64 = u64::BITS as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    FromZero,
    FromAny,
}

#[derive(Clone, Debug)]
pub struct Edges {
    values: Vec<u64>,
}

impl Edges {
    pub fn of(program: &Program) -> Self {
        let half = u64::MAX / 2;
        let mut values = vec![0, 1, half, half + 1, u64::MAX - 1, u64::MAX];
        for constant in &program.constants {
            values.extend(constant.value.checked_sub(1));
            values.push(constant.value);
            values.extend(constant.value.checked_add(1));
        }
        values.sort_unstable();
        values.dedup();
        assert!(!values.is_empty(), "the edges include the ends of u64");
        Self { values }
    }

    pub fn draw(&self, rng: &mut Rng) -> u64 {
        if rng.chance(EDGE_PPM) {
            return self.values[rng.pick(self.values.len())];
        }
        let shift = rng.below(U64_BITS);
        rng.draw() >> shift
    }

    pub fn length(&self, rng: &mut Rng, capacity: u64) -> u64 {
        if rng.chance(EDGE_PPM) {
            let ends = [0, capacity.saturating_sub(1), capacity];
            return ends[rng.pick(ends.len())];
        }
        rng.below(capacity + 1)
    }
}

pub fn draw(program: &Program, ty: &Type, rng: &mut Rng, edges: &Edges) -> Value {
    match ty {
        Type::U64 => Value::U64(edges.draw(rng)),
        Type::Bool => Value::Bool(rng.draw() & 1 == 1),
        Type::Text => Value::Text(SimWorld::generated_text(rng)),
        Type::List { element, capacity } => {
            let length = edges.length(rng, capacity.value);
            Value::List(
                (0..length)
                    .map(|_| draw(program, element, rng, edges))
                    .collect(),
            )
        }
        Type::Sum(_) | Type::Known(_) | Type::Maybe(_) => {
            let variants = ty.variants(program).expect("a sum has variants");
            let variant = rng.pick(variants.len());
            let fields = variants[variant]
                .fields
                .iter()
                .map(|field| draw(program, field, rng, edges))
                .collect();
            Value::Variant(variant, fields)
        }
    }
}

pub fn entry(program: &Program, rng: &mut Rng, edges: &Edges) -> Option<Vec<Value>> {
    for _ in 0..ENTRY_DRAWS_MAX {
        let state: Vec<Value> = program
            .state
            .iter()
            .map(|declared| draw(program, &declared.ty, rng, edges))
            .collect();
        if kept(program, &state, "in the entry state").is_ok() {
            return Some(state);
        }
    }
    None
}

pub fn entry_record(program: &Program, state: &[Value]) -> Record {
    Record::Checkpoint {
        revision: session::ENTRY_REVISION,
        state: program.name.clone(),
        payload: value::state_json(program, state).encode(),
    }
}

fn violation<T>(invariant: Invariant, detail: String) -> Result<T, Violation> {
    Err(Violation { invariant, detail })
}

fn allowed(program: &Program, trap: &str) -> bool {
    let declared = program
        .model
        .as_ref()
        .is_some_and(crate::ir::Model::declared);
    ALLOWED_TRAPS
        .iter()
        .filter(|prefix| !(declared && **prefix == MODEL_REPLY_TRAP))
        .any(|prefix| trap.starts_with(prefix))
}

fn trap_of(outcome: Outcome, result: &Json) -> Option<&str> {
    if outcome != Outcome::Failed {
        return None;
    }
    result.get("trap").and_then(Json::as_str)
}

fn describe(program: &Program, state: &[Value]) -> String {
    let mut shown = Vec::new();
    for (declared, value) in program.state.iter().zip(state) {
        match (&declared.ty, value) {
            (Type::U64, Value::U64(number)) => shown.push(format!("{} = {number}", declared.name)),
            (Type::List { .. }, Value::List(items)) => {
                shown.push(format!("len({}) = {}", declared.name, items.len()))
            }
            _ => {}
        }
    }
    shown.join(", ")
}

fn kept(program: &Program, state: &[Value], when: &str) -> Result<(), Violation> {
    for invariant in &program.invariants {
        let detail = match engine::evaluate(program, state, &[], &invariant.condition) {
            Ok(Value::Bool(true)) => continue,
            Ok(Value::Bool(false)) => {
                format!("invariant '{}' does not hold {when}", invariant.name)
            }
            Ok(other) => panic!("the checker types an invariant as bool, found {other:?}"),
            Err(trap) => format!("invariant '{}' traps with '{trap}' {when}", invariant.name),
        };
        let shown = describe(program, state);
        return violation(Invariant::InvariantKept, format!("{detail}: {shown}"));
    }
    Ok(())
}

fn records_of(folded: &session::Session, records: &[Record]) -> BTreeMap<u64, u64> {
    let mut counts = BTreeMap::new();
    for record in records {
        let turn = match record {
            Record::Recovery { .. } => continue,
            Record::Checkpoint { revision, .. } if *revision == session::ENTRY_REVISION => continue,
            Record::Checkpoint { revision, .. } => *revision,
            Record::Cell { cell, .. } => {
                folded
                    .cell_origins
                    .get(cell)
                    .expect("a folded cell record names a cell with an origin")
                    .0
            }
            other => other
                .turn()
                .expect("every record but a checkpoint names its turn"),
        };
        *counts.entry(turn).or_insert(0) += 1;
    }
    counts
}

fn within_worst(
    program: &Program,
    folded: &session::Session,
    records: &[Record],
) -> Result<(), Violation> {
    let counts = records_of(folded, records);
    for (turn, log) in &folded.turns {
        let handler = program
            .handler(&log.handler)
            .expect("a replayed turn names a handler");
        let written = counts.get(turn).copied().unwrap_or(0);
        let worst = handler.worst.records.total;
        if written > worst {
            return violation(
                Invariant::WorstCase,
                format!(
                    "turn {turn} ({}) wrote {written} journal records besides recoveries, over the worst case of {worst}",
                    log.handler
                ),
            );
        }
    }
    Ok(())
}

pub fn check(
    program: &Program,
    target: &target::Model,
    records: &[Record],
    worst_faults: &[String],
) -> Result<(), Violation> {
    if let Some(fault) = worst_faults.first() {
        return violation(Invariant::WorstCase, fault.clone());
    }
    let folded = session::fold(records).map_err(|error| Violation {
        invariant: Invariant::Liveness,
        detail: format!("the final journal does not fold: {error}"),
    })?;
    within_worst(program, &folded, records)?;
    let mut state = turn::entry_state(program, &folded).map_err(|exit| Violation {
        invariant: Invariant::Liveness,
        detail: format!("the entry checkpoint does not load: {exit:?}"),
    })?;
    kept(program, &state, "in the entry state")?;
    for (turn, log) in &folded.turns {
        let finished = log
            .finished
            .as_ref()
            .expect("the safety invariants require every turn finished");
        if let Some(trap) = trap_of(finished.outcome, &finished.result)
            && !allowed(program, trap)
        {
            return violation(
                Invariant::NoTrap,
                format!("turn {turn} ({}) trapped with '{trap}'", log.handler),
            );
        }
        state = turn::replay(program, target, *turn, log, state).map_err(|exit| Violation {
            invariant: Invariant::Liveness,
            detail: format!("turn {turn} does not replay: {exit:?}"),
        })?;
        kept(
            program,
            &state,
            &format!("after turn {turn} ({})", log.handler),
        )?;
    }
    Ok(())
}
