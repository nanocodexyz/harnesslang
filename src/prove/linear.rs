use std::collections::{BTreeMap, BTreeSet};

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
    pub constraints_max: usize,
}

impl Budget {
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
    numerator.div_euclid(denominator)
}

pub fn ceil_div(numerator: i128, denominator: i128) -> Option<i128> {
    assert!(denominator > 0, "ceiling division by a positive number");
    Some(-(numerator.checked_neg()?.div_euclid(denominator)))
}

fn gcd(mut left: i128, mut right: i128) -> i128 {
    left = left.abs();
    right = right.abs();
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

    fn normalized(mut self) -> Option<Self> {
        let divisor = self.terms.values().fold(0, |acc, value| gcd(acc, *value));
        if divisor > 1 {
            for coefficient in self.terms.values_mut() {
                *coefficient /= divisor;
            }
            self.constant = ceil_div(self.constant, divisor)?;
        }
        Some(self)
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

fn variables(constraints: &BTreeSet<Linear>) -> BTreeSet<Var> {
    constraints
        .iter()
        .flat_map(|constraint| constraint.terms.keys().copied())
        .collect()
}

fn contradiction(constraints: &BTreeSet<Linear>) -> bool {
    constraints
        .iter()
        .any(|constraint| constraint.terms.is_empty() && constraint.constant > 0)
}

fn subsume(constraints: BTreeSet<Linear>) -> BTreeSet<Linear> {
    let mut strongest: BTreeMap<BTreeMap<Var, i128>, i128> = BTreeMap::new();
    for constraint in constraints {
        let entry = strongest
            .entry(constraint.terms)
            .or_insert(constraint.constant);
        *entry = (*entry).max(constraint.constant);
    }
    let set: BTreeSet<Linear> = strongest
        .into_iter()
        .map(|(terms, constant)| Linear { terms, constant })
        .collect();
    let bounds = boxed(&set);
    set.into_iter()
        .filter(|constraint| constraint.terms.len() < 2 || !box_implies(&bounds, constraint))
        .collect()
}

fn boxed(constraints: &BTreeSet<Linear>) -> BTreeMap<Var, Interval> {
    let mut bounds: BTreeMap<Var, Interval> = BTreeMap::new();
    for constraint in constraints {
        let [(var, coefficient)] = constraint.terms.iter().collect::<Vec<_>>()[..] else {
            continue;
        };
        let entry = bounds.entry(*var).or_insert((None, None));
        if *coefficient > 0 {
            let Some(negated) = constraint.constant.checked_neg() else {
                continue;
            };
            let high = floor_div(negated, *coefficient);
            entry.1 = Some(entry.1.map_or(high, |current| current.min(high)));
        } else if let Some(low) = ceil_div(constraint.constant, -*coefficient) {
            entry.0 = Some(entry.0.map_or(low, |current| current.max(low)));
        }
    }
    bounds
}

fn box_implies(bounds: &BTreeMap<Var, Interval>, constraint: &Linear) -> bool {
    let mut highest = Some(constraint.constant);
    for (var, coefficient) in &constraint.terms {
        let (low, high) = bounds.get(var).copied().unwrap_or((None, None));
        let extreme = if *coefficient > 0 { high } else { low };
        highest = highest
            .zip(extreme)
            .and_then(|(total, value)| total.checked_add(coefficient.checked_mul(value)?));
    }
    highest.is_some_and(|value| value <= 0)
}

fn prepare(constraints: &[Linear]) -> Result<BTreeSet<Linear>, Unsure> {
    let mut set = BTreeSet::new();
    for constraint in constraints {
        let normalized = constraint.clone().normalized().ok_or(Unsure::Overflow)?;
        if normalized.terms.is_empty() && normalized.constant <= 0 {
            continue;
        }
        set.insert(normalized);
    }
    Ok(subsume(set))
}

pub fn eliminate(
    constraints: &BTreeSet<Linear>,
    var: Var,
    budget: &mut Budget,
) -> Result<BTreeSet<Linear>, Unsure> {
    let mut kept = BTreeSet::new();
    let mut above = Vec::new();
    let mut below = Vec::new();
    for constraint in constraints {
        match constraint.terms.get(&var).copied() {
            None => {
                kept.insert(constraint.clone());
            }
            Some(coefficient) if coefficient > 0 => above.push((constraint, coefficient)),
            Some(coefficient) => below.push((constraint, -coefficient)),
        }
    }
    for (upper, upper_coefficient) in &above {
        for (lower, lower_coefficient) in &below {
            budget.spend()?;
            let combined = upper
                .scale(*lower_coefficient)
                .and_then(|scaled| scaled.add(&lower.scale(*upper_coefficient)?))
                .and_then(Linear::normalized)
                .ok_or(Unsure::Overflow)?;
            assert!(
                !combined.terms.contains_key(&var),
                "a combination cancels the eliminated variable"
            );
            if combined.terms.is_empty() && combined.constant <= 0 {
                continue;
            }
            kept.insert(combined);
            if kept.len() > budget.constraints_max {
                return Err(Unsure::Budget);
            }
        }
    }
    Ok(subsume(kept))
}

fn cheapest(constraints: &BTreeSet<Linear>, candidates: &BTreeSet<Var>) -> Option<Var> {
    candidates.iter().copied().min_by_key(|var| {
        let mut above = 0_usize;
        let mut below = 0_usize;
        for constraint in constraints {
            match constraint.terms.get(var) {
                Some(coefficient) if *coefficient > 0 => above += 1,
                Some(_) => below += 1,
                None => {}
            }
        }
        above.saturating_mul(below)
    })
}

pub fn project(
    constraints: &[Linear],
    keep: &BTreeSet<Var>,
    budget: &mut Budget,
) -> Result<Vec<Linear>, Unsure> {
    let mut set = prepare(constraints)?;
    let mut doomed: BTreeSet<Var> = variables(&set).difference(keep).copied().collect();
    let rounds_max = doomed.len();
    for _ in 0..rounds_max {
        if contradiction(&set) {
            break;
        }
        let Some(var) = cheapest(&set, &doomed) else {
            break;
        };
        doomed.remove(&var);
        set = eliminate(&set, var, budget)?;
    }
    Ok(set.into_iter().collect())
}

pub fn feasible(constraints: &[Linear], budget: &mut Budget) -> Result<bool, Unsure> {
    let projected = project(constraints, &BTreeSet::new(), budget)?;
    Ok(!projected
        .iter()
        .any(|constraint| constraint.terms.is_empty() && constraint.constant > 0))
}

pub fn implies(facts: &[Linear], goal: &Linear, budget: &mut Budget) -> Result<bool, Unsure> {
    let mut system = facts.to_vec();
    system.push(negation(goal).ok_or(Unsure::Overflow)?);
    Ok(!feasible(&system, budget)?)
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
    let mut system = facts.to_vec();
    let pinned = equal(&Linear::var(fresh), expression).ok_or(Unsure::Overflow)?;
    system.extend(pinned);
    let projected = project(&system, &BTreeSet::from([fresh]), budget)?;
    let mut best: Option<i128> = None;
    for constraint in &projected {
        if constraint.terms.is_empty() {
            if constraint.constant > 0 {
                return Ok(Extent::Infeasible);
            }
            continue;
        }
        let coefficient = constraint.terms[&fresh];
        if coefficient > 0 {
            let bound = floor_div(
                constraint.constant.checked_neg().ok_or(Unsure::Overflow)?,
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

type Interval = (Option<i128>, Option<i128>);

fn bounds_of(
    constraints: &BTreeSet<Linear>,
    var: Var,
    values: &BTreeMap<Var, i128>,
) -> Option<Interval> {
    let mut low: Option<i128> = None;
    let mut high: Option<i128> = None;
    for constraint in constraints {
        let Some(&coefficient) = constraint.terms.get(&var) else {
            continue;
        };
        let mut rest = constraint.clone();
        rest.terms.remove(&var);
        let rest = rest.evaluate(values)?;
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
    let mut set = prepare(constraints)?;
    let mut stages = Vec::new();
    let mut remaining = variables(&set);
    let rounds_max = remaining.len();
    for _ in 0..rounds_max {
        if contradiction(&set) {
            return Ok(None);
        }
        let Some(var) = cheapest(&set, &remaining) else {
            break;
        };
        remaining.remove(&var);
        let next = eliminate(&set, var, budget)?;
        stages.push((var, std::mem::replace(&mut set, next)));
    }
    if contradiction(&set) {
        return Ok(None);
    }
    let mut values = BTreeMap::new();
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
        values.insert(*var, value);
    }
    Ok(Some(values))
}
