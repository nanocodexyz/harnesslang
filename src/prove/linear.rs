mod system;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};

use crate::prove::linear::system::{
    Dense, Hashing, MultiplyRotate, Row, Scratch, cheapest, contradiction, dense, eliminate,
    project_rows,
};

pub type Var = usize;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Linear {
    pub terms: BTreeMap<Var, i128>,
    pub constant: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsure {
    Budget,
    Overflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extent {
    Infeasible,
    Unbounded,
    At(i128),
}

pub struct Budget {
    pub steps_left: u64,
    constraints_max: usize,
    memo: Memo,
    memo_entries_max: usize,
    scratch: Scratch,
}

impl Budget {
    pub fn new(steps_max: u64, constraints_max: usize, memo_entries_max: usize) -> Self {
        Self {
            steps_left: steps_max,
            constraints_max,
            memo: Memo::default(),
            memo_entries_max,
            scratch: Scratch::default(),
        }
    }

    fn spend(&mut self) -> Result<(), Unsure> {
        if self.steps_left == 0 {
            return Err(Unsure::Budget);
        }
        self.steps_left -= 1;
        Ok(())
    }
}

pub fn floor_div(numerator: i128, denominator: i128) -> i128 {
    assert!(denominator > 0, "floor division by a positive number");
    if denominator == 1 {
        return numerator;
    }
    numerator.div_euclid(denominator)
}

pub fn ceil_div(numerator: i128, denominator: i128) -> Option<i128> {
    assert!(denominator > 0, "ceiling division by a positive number");
    if denominator == 1 {
        return numerator.checked_neg().map(|_| numerator);
    }
    Some(-(numerator.checked_neg()?.div_euclid(denominator)))
}

pub(super) fn gcd(mut left: i128, mut right: i128) -> i128 {
    left = left.abs();
    right = right.abs();
    if let (Ok(small_left), Ok(small_right)) = (u64::try_from(left), u64::try_from(right)) {
        return i128::from(gcd_words(small_left, small_right));
    }
    while right != 0 {
        let rest = left % right;
        left = right;
        right = rest;
    }
    left
}

fn gcd_words(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let rest = left % right;
        left = right;
        right = rest;
    }
    left
}

impl Linear {
    pub fn constant(value: i128) -> Self {
        Self {
            terms: BTreeMap::new(),
            constant: value,
        }
    }

    pub fn var(var: Var) -> Self {
        Self {
            terms: BTreeMap::from([(var, 1)]),
            constant: 0,
        }
    }

    pub fn as_constant(&self) -> Option<i128> {
        self.terms.is_empty().then_some(self.constant)
    }

    pub fn add(&self, other: &Self) -> Option<Self> {
        let mut terms = self.terms.clone();
        for (var, coefficient) in &other.terms {
            let sum = terms
                .get(var)
                .copied()
                .unwrap_or(0)
                .checked_add(*coefficient)?;
            if sum == 0 {
                terms.remove(var);
            } else {
                terms.insert(*var, sum);
            }
        }
        Some(Self {
            terms,
            constant: self.constant.checked_add(other.constant)?,
        })
    }

    pub fn scale(&self, factor: i128) -> Option<Self> {
        if factor == 0 {
            return Some(Self::constant(0));
        }
        let mut terms = BTreeMap::new();
        for (var, coefficient) in &self.terms {
            terms.insert(*var, coefficient.checked_mul(factor)?);
        }
        Some(Self {
            terms,
            constant: self.constant.checked_mul(factor)?,
        })
    }

    pub fn subtract(&self, other: &Self) -> Option<Self> {
        self.add(&other.scale(-1)?)
    }

    pub fn plus(&self, value: i128) -> Option<Self> {
        self.add(&Self::constant(value))
    }

    pub fn evaluate(&self, values: &BTreeMap<Var, i128>) -> Option<i128> {
        let mut total = self.constant;
        for (var, coefficient) in &self.terms {
            let value = values.get(var).copied().unwrap_or(0);
            total = total.checked_add(coefficient.checked_mul(value)?)?;
        }
        Some(total)
    }
}

pub fn at_most(left: &Linear, right: &Linear) -> Option<Linear> {
    left.subtract(right)
}

pub fn equal(left: &Linear, right: &Linear) -> Option<[Linear; 2]> {
    Some([left.subtract(right)?, right.subtract(left)?])
}

pub fn negation(constraint: &Linear) -> Option<Linear> {
    constraint.scale(-1)?.plus(1)
}

type Memo = HashMap<Key, Outcome, Hashing>;

#[derive(PartialEq, Eq)]
struct Key {
    hash: u64,
    words: Vec<i128>,
}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

struct Outcome {
    projected: Result<Vec<Row>, Unsure>,
    steps: u64,
}

fn length_of(items: usize) -> i128 {
    i128::try_from(items).expect("a length fits i128")
}

fn projection_key(rows: &[Row], doomed: &[Var]) -> Key {
    let terms: usize = rows.iter().map(|row| row.terms.len()).sum();
    let mut key = Vec::with_capacity(2 + 2 * rows.len() + 2 * terms + doomed.len());
    key.push(length_of(rows.len()));
    for row in rows {
        key.push(length_of(row.terms.len()));
        for (var, coefficient) in &row.terms {
            key.push(length_of(*var));
            key.push(*coefficient);
        }
        key.push(row.constant);
    }
    key.push(length_of(doomed.len()));
    key.extend(doomed.iter().map(|var| length_of(*var)));
    let mut hasher = MultiplyRotate::default();
    for word in &key {
        hasher.write_i128(*word);
    }
    Key {
        hash: hasher.finish(),
        words: key,
    }
}

impl Budget {
    fn projected(&mut self, system: Dense, keep: &BTreeSet<Var>) -> Result<Vec<Row>, Unsure> {
        let Dense { rows, names } = system;
        let doomed: Vec<Var> = (0..names.len())
            .filter(|index| !keep.contains(&names[*index]))
            .collect();
        let key = projection_key(&rows, &doomed);
        let restore = |rows: &Vec<Row>| rows.iter().map(|row| row.restored(&names)).collect();
        if let Some(outcome) = self.memo.get(&key)
            && outcome.steps <= self.steps_left
        {
            self.steps_left -= outcome.steps;
            return outcome
                .projected
                .as_ref()
                .map(restore)
                .map_err(|unsure| *unsure);
        }
        let steps_before = self.steps_left;
        let projected = project_rows(rows, doomed, names.len(), self);
        let steps = steps_before - self.steps_left;
        let restored = projected.as_ref().map(restore).map_err(|unsure| *unsure);
        if projected != Err(Unsure::Budget) && self.memo.len() < self.memo_entries_max {
            self.memo.insert(key, Outcome { projected, steps });
        }
        restored
    }
}

fn projected_rows(
    constraints: &[Linear],
    extra: &[Linear],
    keep: &BTreeSet<Var>,
    budget: &mut Budget,
) -> Result<Vec<Row>, Unsure> {
    let system = dense(constraints, extra, &mut budget.scratch)?;
    budget.projected(system, keep)
}

pub fn project(
    constraints: &[Linear],
    keep: &BTreeSet<Var>,
    budget: &mut Budget,
) -> Result<Vec<Linear>, Unsure> {
    let rows = projected_rows(constraints, &[], keep, budget)?;
    Ok(rows.iter().map(Row::linear).collect())
}

pub fn feasible(constraints: &[Linear], budget: &mut Budget) -> Result<bool, Unsure> {
    let projected = projected_rows(constraints, &[], &BTreeSet::new(), budget)?;
    Ok(!contradiction(&projected))
}

pub fn implies(facts: &[Linear], goal: &Linear, budget: &mut Budget) -> Result<bool, Unsure> {
    let negated = [negation(goal).ok_or(Unsure::Overflow)?];
    let projected = projected_rows(facts, &negated, &BTreeSet::new(), budget)?;
    Ok(contradiction(&projected))
}

pub fn connected(facts: &[Linear], seeds: &Linear) -> Vec<Linear> {
    let mut holding: BTreeMap<Var, Vec<usize>> = BTreeMap::new();
    for (index, fact) in facts.iter().enumerate() {
        for var in fact.terms.keys() {
            holding.entry(*var).or_default().push(index);
        }
    }
    let mut taken = vec![false; facts.len()];
    let mut reached: BTreeSet<Var> = seeds.terms.keys().copied().collect();
    let mut frontier: Vec<Var> = reached.iter().copied().collect();
    let visits_max = holding.len() + reached.len();
    for _ in 0..=visits_max {
        let Some(var) = frontier.pop() else {
            break;
        };
        for index in holding.get(&var).into_iter().flatten() {
            if std::mem::replace(&mut taken[*index], true) {
                continue;
            }
            for other in facts[*index].terms.keys() {
                if reached.insert(*other) {
                    frontier.push(*other);
                }
            }
        }
    }
    assert!(
        frontier.is_empty(),
        "every reached variable is visited once"
    );
    facts
        .iter()
        .zip(&taken)
        .filter(|(fact, taken)| **taken || fact.terms.is_empty())
        .map(|(fact, _)| fact.clone())
        .collect()
}

pub fn implies_split(facts: &[Linear], goal: &Linear, budget: &mut Budget) -> Result<bool, Unsure> {
    let near = connected(facts, goal);
    if near.len() < facts.len() && implies(&near, goal, budget)? {
        return Ok(true);
    }
    implies(facts, goal, budget)
}

pub fn implies_feasible(
    facts: &[Linear],
    goal: &Linear,
    budget: &mut Budget,
) -> Result<bool, Unsure> {
    implies(&connected(facts, goal), goal, budget)
}

pub fn range(
    facts: &[Linear],
    expression: &Linear,
    fresh: Var,
    budget: &mut Budget,
) -> Result<(Extent, Extent), Unsure> {
    assert!(
        !expression.terms.contains_key(&fresh),
        "the measuring variable is fresh in the expression"
    );
    let near = connected(facts, expression);
    assert!(
        near.iter().all(|fact| !fact.terms.contains_key(&fresh)),
        "the measuring variable is fresh in the facts"
    );
    let pinned = equal(&Linear::var(fresh), expression).ok_or(Unsure::Overflow)?;
    let projected = projected_rows(&near, &pinned, &BTreeSet::from([fresh]), budget)?;
    let mut low: Option<i128> = None;
    let mut high: Option<i128> = None;
    for row in &projected {
        if row.terms.is_empty() {
            if row.constant > 0 {
                return Ok((Extent::Infeasible, Extent::Infeasible));
            }
            continue;
        }
        let coefficient = row.coefficient(fresh).expect("only the measure is left");
        if coefficient > 0 {
            let bound = floor_div(
                row.constant.checked_neg().ok_or(Unsure::Overflow)?,
                coefficient,
            );
            high = Some(high.map_or(bound, |current| current.min(bound)));
        } else {
            let bound = ceil_div(row.constant, -coefficient).ok_or(Unsure::Overflow)?;
            low = Some(low.map_or(bound, |current| current.max(bound)));
        }
    }
    let shown = |bound: Option<i128>| bound.map_or(Extent::Unbounded, Extent::At);
    Ok((shown(low), shown(high)))
}

pub fn upper(
    facts: &[Linear],
    expression: &Linear,
    fresh: Var,
    budget: &mut Budget,
) -> Result<Extent, Unsure> {
    assert!(
        facts.iter().all(|fact| !fact.terms.contains_key(&fresh)),
        "the measuring variable is fresh in the facts"
    );
    assert!(
        !expression.terms.contains_key(&fresh),
        "the measuring variable is fresh in the expression"
    );
    let pinned = equal(&Linear::var(fresh), expression).ok_or(Unsure::Overflow)?;
    let projected = projected_rows(facts, &pinned, &BTreeSet::from([fresh]), budget)?;
    let mut best: Option<i128> = None;
    for row in &projected {
        if row.terms.is_empty() {
            if row.constant > 0 {
                return Ok(Extent::Infeasible);
            }
            continue;
        }
        let coefficient = row.coefficient(fresh).expect("only the measure is left");
        if coefficient > 0 {
            let bound = floor_div(
                row.constant.checked_neg().ok_or(Unsure::Overflow)?,
                coefficient,
            );
            best = Some(best.map_or(bound, |current| current.min(bound)));
        }
    }
    Ok(best.map_or(Extent::Unbounded, Extent::At))
}

pub fn lower(
    facts: &[Linear],
    expression: &Linear,
    fresh: Var,
    budget: &mut Budget,
) -> Result<Extent, Unsure> {
    let negated = expression.scale(-1).ok_or(Unsure::Overflow)?;
    Ok(match upper(facts, &negated, fresh, budget)? {
        Extent::At(value) => Extent::At(value.checked_neg().ok_or(Unsure::Overflow)?),
        other => other,
    })
}

pub type Interval = (Option<i128>, Option<i128>);

fn bounds_of(rows: &[Row], var: Var, values: &[i128]) -> Option<Interval> {
    let mut low: Option<i128> = None;
    let mut high: Option<i128> = None;
    for row in rows {
        let Some(coefficient) = row.coefficient(var) else {
            continue;
        };
        let mut rest = row.constant;
        for (other, other_coefficient) in &row.terms {
            if *other != var {
                rest = rest.checked_add(other_coefficient.checked_mul(values[*other])?)?;
            }
        }
        if coefficient > 0 {
            let bound = floor_div(rest.checked_neg()?, coefficient);
            high = Some(high.map_or(bound, |current| current.min(bound)));
        } else {
            let bound = ceil_div(rest, -coefficient)?;
            low = Some(low.map_or(bound, |current| current.max(bound)));
        }
    }
    Some((low, high))
}

pub fn model(
    constraints: &[Linear],
    budget: &mut Budget,
) -> Result<Option<BTreeMap<Var, i128>>, Unsure> {
    let Some(values) = candidate(constraints, budget)? else {
        return Ok(None);
    };
    let satisfied = constraints
        .iter()
        .all(|constraint| constraint.evaluate(&values).is_some_and(|value| value <= 0));
    Ok(satisfied.then_some(values))
}

fn candidate(
    constraints: &[Linear],
    budget: &mut Budget,
) -> Result<Option<BTreeMap<Var, i128>>, Unsure> {
    let Dense { mut rows, names } = dense(constraints, &[], &mut budget.scratch)?;
    let vars = names.len();
    let mut stages = Vec::with_capacity(vars);
    let mut remaining: Vec<Var> = (0..vars).collect();
    for _ in 0..vars {
        if contradiction(&rows) {
            return Ok(None);
        }
        let Some(var) = cheapest(&rows, &remaining, vars, &mut budget.scratch) else {
            break;
        };
        remaining.retain(|candidate| *candidate != var);
        let next = eliminate(rows.clone(), var, vars, budget)?;
        stages.push((var, std::mem::replace(&mut rows, next)));
    }
    if contradiction(&rows) {
        return Ok(None);
    }
    let mut values = vec![0; vars];
    let mut assigned = BTreeMap::new();
    for (var, stage) in stages.iter().rev() {
        let Some((low, high)) = bounds_of(stage, *var, &values) else {
            return Ok(None);
        };
        let value = match (low, high) {
            (Some(low), Some(high)) if low > high => return Ok(None),
            (Some(low), _) => low,
            (None, Some(high)) => high.min(0),
            (None, None) => 0,
        };
        values[*var] = value;
        assigned.insert(names[*var], value);
    }
    Ok(Some(assigned))
}
