use crate::check::climb::{
    CYCLE_BASELINE, CYCLE_BASELINE_SCORE, CYCLE_CANDIDATE, CYCLE_CANDIDATE_SCORE, CYCLE_COST,
    CYCLE_FIELDS, CYCLE_INDEX, CYCLE_SPENT, CYCLE_TRIALS, CYCLE_VERDICT, CYCLE_WINS, VERDICT_KEPT,
};
use crate::ir::{Climb, ClimbRule, Program, Type};
use crate::run::journal::Record;
use crate::run::session;
use crate::run::turn;
use crate::run::value::Value;
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::world::SimWorld;

struct Row<'v> {
    index: u64,
    baseline: &'v Value,
    candidate: &'v Value,
    base: u64,
    cand: u64,
    wins: u64,
    pairs: u64,
    cost: u64,
    spent: u64,
    kept: bool,
}

fn number(value: &Value) -> u64 {
    match value {
        Value::U64(number) => *number,
        other => panic!("a cycle's counter is a u64, found {other:?}"),
    }
}

fn kept_variant(program: &Program, ledger: &Type) -> usize {
    let Type::List { element, .. } = ledger else {
        panic!("a climb's ledger is a list");
    };
    let cycle = element.variants(program).expect("a ledger row is a cycle");
    assert_eq!(cycle.len(), 1, "the cycle type has one variant");
    assert_eq!(
        cycle[0].fields.len(),
        CYCLE_FIELDS,
        "a cycle carries its fields"
    );
    let verdicts = cycle[0].fields[CYCLE_VERDICT]
        .variants(program)
        .expect("a cycle's verdict is a sum");
    verdicts
        .iter()
        .position(|variant| variant.name == VERDICT_KEPT)
        .expect("the verdict type names kept")
}

fn rows<'v>(program: &Program, climb: &Climb, state: &'v [Value]) -> Vec<Row<'v>> {
    let kept = kept_variant(program, &program.state[climb.ledger].ty);
    let Value::List(items) = &state[climb.ledger] else {
        panic!("a climb's ledger holds a list");
    };
    items
        .iter()
        .map(|item| {
            let Value::Variant(_, fields) = item else {
                panic!("a ledger row is a cycle value");
            };
            assert_eq!(
                fields.len(),
                CYCLE_FIELDS,
                "a ledger row carries its fields"
            );
            let Value::Variant(verdict, _) = &fields[CYCLE_VERDICT] else {
                panic!("a cycle's verdict is a variant");
            };
            Row {
                index: number(&fields[CYCLE_INDEX]),
                baseline: &fields[CYCLE_BASELINE],
                candidate: &fields[CYCLE_CANDIDATE],
                base: number(&fields[CYCLE_BASELINE_SCORE]),
                cand: number(&fields[CYCLE_CANDIDATE_SCORE]),
                wins: number(&fields[CYCLE_WINS]),
                pairs: number(&fields[CYCLE_TRIALS]),
                cost: number(&fields[CYCLE_COST]),
                spent: number(&fields[CYCLE_SPENT]),
                kept: *verdict == kept,
            }
        })
        .collect()
}

fn beats(climb: &Climb, row: &Row<'_>) -> Option<bool> {
    let (base, cand) = (u128::from(row.base), u128::from(row.cand));
    let either = |margin: u128| {
        if climb.higher {
            cand > base + margin
        } else {
            cand + margin < base
        }
    };
    match climb.rule {
        ClimbRule::Mean(margin) => Some(either(u128::from(climb.trials) * u128::from(margin))),
        ClimbRule::Best(margin) => Some(either(u128::from(margin))),
        ClimbRule::Sign(wins) => Some(row.wins >= wins),
        ClimbRule::When => None,
    }
}

fn failed<T>(invariant: Invariant, climb: &Climb, detail: String) -> Result<T, Violation> {
    Err(Violation {
        invariant,
        detail: format!("climb '{}': {detail}", climb.name),
    })
}

fn left<'v>(row: &Row<'v>) -> &'v Value {
    if row.kept {
        row.candidate
    } else {
        row.baseline
    }
}

fn kept_valid(climb: &Climb, position: usize, row: &Row<'_>) -> Result<(), Violation> {
    if !row.kept {
        return Ok(());
    }
    if row.pairs != climb.trials {
        return failed(
            Invariant::ClimbKeptValid,
            climb,
            format!(
                "ledger row {position} is kept after {} of {} paired trials",
                row.pairs, climb.trials
            ),
        );
    }
    if beats(climb, row) == Some(false) {
        return failed(
            Invariant::ClimbKeptValid,
            climb,
            format!(
                "ledger row {position} is kept although candidate {} does not beat baseline {} under the accept rule",
                row.cand, row.base
            ),
        );
    }
    Ok(())
}

fn complete(climb: &Climb, rows: &[Row<'_>], state: &[Value]) -> Result<(), Violation> {
    let mut logged = 0;
    for (position, row) in rows.iter().enumerate() {
        if row.index != 1 && row.index != logged + 1 {
            return failed(
                Invariant::ClimbLedgerComplete,
                climb,
                format!(
                    "ledger row {position} is evaluation {} but the row before it is evaluation {logged}: an evaluated candidate is missing from the ledger",
                    row.index
                ),
            );
        }
        logged = row.index;
    }
    let evaluated = number(&state[climb.evaluated]);
    if evaluated > 0 && evaluated != logged {
        return failed(
            Invariant::ClimbLedgerComplete,
            climb,
            format!(
                "{evaluated} candidate(s) were evaluated but the ledger's last row is evaluation {logged}"
            ),
        );
    }
    Ok(())
}

fn one(program: &Program, climb: &Climb, state: &[Value]) -> Result<(), Violation> {
    let rows = rows(program, climb, state);
    complete(climb, &rows, state)?;
    let mut previous: Option<&Row<'_>> = None;
    let mut run_cost: u64 = 0;
    for (position, row) in rows.iter().enumerate() {
        if row.index == 1 {
            run_cost = 0;
        }
        run_cost += row.cost;
        if climb.budget.is_some() && run_cost != row.spent {
            return failed(
                Invariant::ClimbCostBalanced,
                climb,
                format!(
                    "ledger row {position} says {} was spent, but its run's rows cost {run_cost}",
                    row.spent
                ),
            );
        }
        kept_valid(climb, position, row)?;
        if let Some(last) = previous
            && row.baseline != left(last)
        {
            return failed(
                Invariant::ClimbStateFollows,
                climb,
                format!(
                    "ledger row {position} starts from {:?}, but the row before it left {:?}",
                    row.baseline,
                    left(last)
                ),
            );
        }
        previous = Some(row);
    }
    if let Some(last) = previous
        && &state[climb.over] != left(last)
    {
        return failed(
            Invariant::ClimbStateFollows,
            climb,
            format!(
                "the climbed state holds {:?}, but the ledger's last row left {:?}",
                state[climb.over],
                left(last)
            ),
        );
    }
    Ok(())
}

pub fn check(program: &Program, world: &SimWorld<'_>, records: &[Record]) -> Result<(), Violation> {
    if program.climbs.is_empty() {
        return Ok(());
    }
    let session = session::fold(records).expect("liveness folded this journal");
    let state = turn::replay_all(program, &world.target, &session)
        .expect("replay_equal replayed this journal");
    for climb in &program.climbs {
        one(program, climb, &state)?;
    }
    Ok(())
}
