use crate::ir::{Program, Type};
use crate::run::engine;
use crate::run::journal::{Outcome, Record};
use crate::run::json::Json;
use crate::run::session;
use crate::run::turn;
use crate::run::value::{self, Value};
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::rng::Rng;
use crate::target;

pub const TURNS_PER_SESSION_MAX: u64 = 12;
pub const OVER_CAPACITY_PPM: u64 = 50_000;
pub const EDGE_PPM: u64 = 500_000;
pub const ALLOWED_TRAPS: [&str; 2] = ["requires(", "model reply: "];
const U64_BITS: u64 = u64::BITS as u64;

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
}

fn violation<T>(invariant: Invariant, detail: String) -> Result<T, Violation> {
    Err(Violation { invariant, detail })
}

fn allowed(trap: &str) -> bool {
    ALLOWED_TRAPS.iter().any(|prefix| trap.starts_with(prefix))
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

pub fn check(
    program: &Program,
    target: &target::Model,
    records: &[Record],
) -> Result<(), Violation> {
    let folded = session::fold(records).map_err(|error| Violation {
        invariant: Invariant::Liveness,
        detail: format!("the final journal does not fold: {error}"),
    })?;
    let mut state = value::initial_state(program);
    kept(program, &state, "in the zero state")?;
    for (turn, log) in &folded.turns {
        let finished = log
            .finished
            .as_ref()
            .expect("the safety invariants require every turn finished");
        if let Some(trap) = trap_of(finished.outcome, &finished.result)
            && !allowed(trap)
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
