use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hasher};

use crate::prove::linear::{Budget, Interval, Linear, Unsure, Var, ceil_div, floor_div, gcd};

const FX_MULTIPLIER: u64 = 0x517c_c1b7_2722_0a95;
const FX_ROTATION_BITS: u32 = 5;

#[derive(Default)]
pub struct MultiplyRotate {
    hash: u64,
}

impl Hasher for MultiplyRotate {
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(size_of::<u64>());
        for chunk in &mut chunks {
            self.write_u64(u64::from_le_bytes(
                chunk.try_into().expect("an exact chunk is a word"),
            ));
        }
        for byte in chunks.remainder() {
            self.write_u64(u64::from(*byte));
        }
    }

    fn write_u64(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(FX_ROTATION_BITS) ^ word).wrapping_mul(FX_MULTIPLIER);
    }

    fn write_u128(&mut self, word: u128) {
        self.write_u64(word as u64);
        self.write_u64((word >> u64::BITS) as u64);
    }

    fn write_usize(&mut self, word: usize) {
        self.write_u64(word as u64);
    }

    fn write_i128(&mut self, word: i128) {
        self.write_u128(word as u128);
    }

    fn finish(&self) -> u64 {
        self.hash
    }
}

pub type Hashing = BuildHasherDefault<MultiplyRotate>;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Row {
    pub terms: Vec<(Var, i128)>,
    pub constant: i128,
}

impl Row {
    pub fn of(linear: &Linear) -> Self {
        let terms: Vec<(Var, i128)> = linear.terms.iter().map(|(var, c)| (*var, *c)).collect();
        assert!(
            terms.iter().all(|(_, coefficient)| *coefficient != 0),
            "a linear term has a nonzero coefficient"
        );
        Self {
            terms,
            constant: linear.constant,
        }
    }

    pub fn linear(&self) -> Linear {
        Linear {
            terms: self.terms.iter().copied().collect(),
            constant: self.constant,
        }
    }

    pub fn trivial(&self) -> bool {
        self.terms.is_empty() && self.constant <= 0
    }

    pub fn absurd(&self) -> bool {
        self.terms.is_empty() && self.constant > 0
    }

    pub fn coefficient(&self, var: Var) -> Option<i128> {
        let index = self.terms.binary_search_by_key(&var, |term| term.0).ok()?;
        Some(self.terms[index].1)
    }

    pub fn normalized(mut self) -> Option<Self> {
        let divisor = self.terms.iter().fold(0, |acc, term| gcd(acc, term.1));
        if divisor > 1 {
            for term in &mut self.terms {
                term.1 /= divisor;
            }
            self.constant = ceil_div(self.constant, divisor)?;
        }
        Some(self)
    }

    pub fn renamed(&self, names: &[Var]) -> Self {
        let rename = |var: Var| {
            names
                .binary_search(&var)
                .expect("a row's variable is named")
        };
        Self {
            terms: self
                .terms
                .iter()
                .map(|(var, c)| (rename(*var), *c))
                .collect(),
            constant: self.constant,
        }
    }

    pub fn restored(&self, names: &[Var]) -> Self {
        Self {
            terms: self
                .terms
                .iter()
                .map(|(index, c)| (names[*index], *c))
                .collect(),
            constant: self.constant,
        }
    }
}

fn scaled_term(term: (Var, i128), factor: i128) -> Option<(Var, i128)> {
    Some((term.0, term.1.checked_mul(factor)?))
}

fn combine(upper: &Row, upper_scale: i128, lower: &Row, lower_scale: i128) -> Option<Row> {
    let terms_max = upper.terms.len() + lower.terms.len();
    let mut terms = Vec::with_capacity(terms_max);
    let mut left = upper.terms.iter().copied().peekable();
    let mut right = lower.terms.iter().copied().peekable();
    for _ in 0..terms_max {
        let term = match (left.peek().copied(), right.peek().copied()) {
            (Some((var, from_upper)), Some((other, from_lower))) if var == other => {
                left.next();
                right.next();
                let sum = from_upper
                    .checked_mul(upper_scale)?
                    .checked_add(from_lower.checked_mul(lower_scale)?)?;
                (var, sum)
            }
            (Some(term), Some((other, _))) if term.0 < other => {
                left.next();
                scaled_term(term, upper_scale)?
            }
            (Some(term), None) => {
                left.next();
                scaled_term(term, upper_scale)?
            }
            (_, Some(term)) => {
                right.next();
                scaled_term(term, lower_scale)?
            }
            (None, None) => break,
        };
        if term.1 != 0 {
            terms.push(term);
        }
    }
    let constant = upper
        .constant
        .checked_mul(upper_scale)?
        .checked_add(lower.constant.checked_mul(lower_scale)?)?;
    Some(Row { terms, constant })
}

pub fn variables(rows: &[Row]) -> Vec<Var> {
    let mut vars: Vec<Var> = rows
        .iter()
        .flat_map(|row| row.terms.iter().map(|term| term.0))
        .collect();
    vars.sort_unstable();
    vars.dedup();
    vars
}

pub fn contradiction(rows: &[Row]) -> bool {
    rows.iter().any(Row::absurd)
}

pub fn subsume(mut rows: Vec<Row>, vars: usize) -> Vec<Row> {
    rows.sort_unstable();
    let mut strongest: Vec<Row> = Vec::with_capacity(rows.len());
    for row in rows {
        match strongest.last_mut() {
            Some(last) if last.terms == row.terms => {
                last.constant = last.constant.max(row.constant)
            }
            _ => strongest.push(row),
        }
    }
    let bounds = boxed(&strongest, vars);
    strongest.retain(|row| row.terms.len() < 2 || !box_implies(&bounds, row));
    strongest
}

fn boxed(rows: &[Row], vars: usize) -> Vec<Interval> {
    let mut bounds: Vec<Interval> = vec![(None, None); vars];
    for row in rows {
        let [(var, coefficient)] = row.terms[..] else {
            continue;
        };
        let entry = &mut bounds[var];
        if coefficient > 0 {
            let Some(negated) = row.constant.checked_neg() else {
                continue;
            };
            let high = floor_div(negated, coefficient);
            entry.1 = Some(entry.1.map_or(high, |current| current.min(high)));
        } else if let Some(low) = ceil_div(row.constant, -coefficient) {
            entry.0 = Some(entry.0.map_or(low, |current| current.max(low)));
        }
    }
    bounds
}

fn box_implies(bounds: &[Interval], row: &Row) -> bool {
    let mut highest = row.constant;
    for (var, coefficient) in &row.terms {
        let (low, high) = bounds[*var];
        let extreme = if *coefficient > 0 { high } else { low };
        let Some(value) = extreme else {
            return false;
        };
        let Some(total) = coefficient
            .checked_mul(value)
            .and_then(|term| highest.checked_add(term))
        else {
            return false;
        };
        highest = total;
    }
    highest <= 0
}

pub fn prepare(rows: Vec<Row>, vars: usize) -> Result<Vec<Row>, Unsure> {
    let mut prepared = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row.normalized().ok_or(Unsure::Overflow)?;
        if !row.trivial() {
            prepared.push(row);
        }
    }
    Ok(subsume(prepared, vars))
}

pub fn eliminate(
    rows: Vec<Row>,
    var: Var,
    vars: usize,
    budget: &mut Budget,
) -> Result<Vec<Row>, Unsure> {
    let mut kept = Vec::with_capacity(rows.len());
    let mut above = Vec::new();
    let mut below = Vec::new();
    for row in rows {
        match row.coefficient(var) {
            None => kept.push(row),
            Some(coefficient) if coefficient > 0 => above.push((row, coefficient)),
            Some(coefficient) => below.push((row, -coefficient)),
        }
    }
    let pairs = above.len() * below.len();
    let mut distinct: Option<HashSet<Row, Hashing>> =
        (kept.len() + pairs > budget.constraints_max).then(|| kept.iter().cloned().collect());
    for (upper, upper_coefficient) in &above {
        for (lower, lower_coefficient) in &below {
            budget.spend()?;
            let combined = combine(upper, *lower_coefficient, lower, *upper_coefficient)
                .and_then(Row::normalized)
                .ok_or(Unsure::Overflow)?;
            assert!(
                combined.coefficient(var).is_none(),
                "a combination cancels the eliminated variable"
            );
            if combined.trivial() {
                continue;
            }
            if let Some(distinct) = &mut distinct {
                distinct.insert(combined.clone());
                if distinct.len() > budget.constraints_max {
                    return Err(Unsure::Budget);
                }
            }
            kept.push(combined);
        }
    }
    Ok(subsume(kept, vars))
}

pub fn cheapest(rows: &[Row], candidates: &[Var], vars: usize) -> Option<Var> {
    let mut counts = vec![(0_usize, 0_usize); vars];
    for row in rows {
        for (var, coefficient) in &row.terms {
            if *coefficient > 0 {
                counts[*var].0 += 1;
            } else {
                counts[*var].1 += 1;
            }
        }
    }
    candidates
        .iter()
        .min_by_key(|var| counts[**var].0.saturating_mul(counts[**var].1))
        .copied()
}

pub fn project_rows(
    mut rows: Vec<Row>,
    mut doomed: Vec<Var>,
    vars: usize,
    budget: &mut Budget,
) -> Result<Vec<Row>, Unsure> {
    let rounds_max = doomed.len();
    for _ in 0..rounds_max {
        if contradiction(&rows) {
            break;
        }
        let Some(var) = cheapest(&rows, &doomed, vars) else {
            break;
        };
        doomed.retain(|candidate| *candidate != var);
        rows = eliminate(rows, var, vars, budget)?;
    }
    Ok(rows)
}

pub struct Dense {
    pub rows: Vec<Row>,
    pub names: Vec<Var>,
}

pub fn dense(constraints: &[Linear]) -> Result<Dense, Unsure> {
    let rows: Vec<Row> = constraints.iter().map(Row::of).collect();
    let names = variables(&rows);
    let renamed = rows.iter().map(|row| row.renamed(&names)).collect();
    let prepared = prepare(renamed, names.len())?;
    let present = variables(&prepared);
    if present.len() == names.len() {
        return Ok(Dense {
            rows: prepared,
            names,
        });
    }
    Ok(Dense {
        rows: prepared.iter().map(|row| row.renamed(&present)).collect(),
        names: present.iter().map(|index| names[*index]).collect(),
    })
}
