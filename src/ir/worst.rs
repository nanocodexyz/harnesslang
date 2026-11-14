use crate::ir::{Bound, Handler, SiteKind, Stmt};

pub const ADMISSION_RECORDS: u64 = 1;
pub const FINISH_RECORDS: u64 = 2;
pub const EFFECT_RECORDS: u64 = 2;
pub const MODEL_RECORDS: u64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Factor {
    pub name: String,
    pub bound: Bound,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    pub total: u64,
    pub loops: Vec<Factor>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorstCase {
    pub effects: Cost,
    pub records: Cost,
    pub requests: Cost,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overflow {
    pub measure: &'static str,
    pub loops: Vec<Factor>,
}

pub const MEASURES: [&str; 3] = ["effects", "journal records", "outbound requests"];

type Costs = [Cost; 3];

impl Cost {
    fn of(total: u64) -> Self {
        Self {
            total,
            loops: Vec::new(),
        }
    }

    fn then(self, other: Self) -> Option<Self> {
        let total = self.total.checked_add(other.total)?;
        let loops = if other.total > self.total {
            other.loops
        } else {
            self.loops
        };
        Some(Self { total, loops })
    }

    fn or(self, other: Self) -> Self {
        if other.total > self.total {
            other
        } else {
            self
        }
    }

    fn times(self, factor: &Factor) -> Option<Self> {
        let total = self.total.checked_mul(factor.bound.value)?;
        if self.total == 0 {
            return Some(self);
        }
        let mut loops = vec![factor.clone()];
        loops.extend(self.loops);
        Some(Self { total, loops })
    }

    pub fn describe_loops(&self) -> String {
        if self.loops.is_empty() {
            return "no loops".to_owned();
        }
        self.loops
            .iter()
            .map(|factor| {
                format!(
                    "{} below {} ({})",
                    factor.name, factor.bound.name, factor.bound.value
                )
            })
            .collect::<Vec<_>>()
            .join(" x ")
    }
}

fn zero() -> Costs {
    [Cost::default(), Cost::default(), Cost::default()]
}

fn combine(left: Costs, right: Costs, factor: Option<&Factor>) -> Result<Costs, Overflow> {
    let mut out = zero();
    for (index, (left, right)) in left.into_iter().zip(right).enumerate() {
        let combined = left.clone().then(right.clone()).ok_or_else(|| Overflow {
            measure: MEASURES[index],
            loops: left.loops.clone(),
        })?;
        out[index] = match factor {
            None => combined,
            Some(factor) => combined.clone().times(factor).ok_or_else(|| {
                let mut loops = vec![factor.clone()];
                loops.extend(combined.loops.clone());
                Overflow {
                    measure: MEASURES[index],
                    loops,
                }
            })?,
        };
    }
    Ok(out)
}

fn widest(left: Costs, right: Costs) -> Costs {
    let [a, b, c] = left;
    let [x, y, z] = right;
    [a.or(x), b.or(y), c.or(z)]
}

fn block(handler: &Handler, statements: &[Stmt]) -> Result<Costs, Overflow> {
    let mut total = zero();
    for statement in statements {
        let cost = statement_cost(handler, statement)?;
        total = combine(total, cost, None)?;
    }
    Ok(total)
}

fn statement_cost(handler: &Handler, statement: &Stmt) -> Result<Costs, Overflow> {
    match statement {
        Stmt::Let { .. } | Stmt::Assign { .. } | Stmt::Push { .. } => Ok(zero()),
        Stmt::Effect { site, .. } => {
            let kind = handler.sites[*site].kind;
            let records = if kind == SiteKind::Model {
                MODEL_RECORDS
            } else {
                EFFECT_RECORDS
            };
            Ok([
                Cost::of(1),
                Cost::of(records),
                Cost::of(u64::from(kind.outbound())),
            ])
        }
        Stmt::Finish { .. } => Ok([Cost::of(0), Cost::of(FINISH_RECORDS), Cost::of(0)]),
        Stmt::If {
            then, otherwise, ..
        } => Ok(widest(block(handler, then)?, block(handler, otherwise)?)),
        Stmt::Match { arms, .. } => {
            let mut widest_arm = zero();
            for arm in arms {
                widest_arm = widest(widest_arm, block(handler, &arm.body)?);
            }
            Ok(widest_arm)
        }
        Stmt::Loop { index, bound, body } => {
            let factor = Factor {
                name: handler.locals[*index].name.clone(),
                bound: bound.clone(),
            };
            combine(zero(), block(handler, body)?, Some(&factor))
        }
        Stmt::For {
            item, bound, body, ..
        } => {
            let factor = Factor {
                name: handler.locals[*item].name.clone(),
                bound: bound.clone(),
            };
            combine(zero(), block(handler, body)?, Some(&factor))
        }
    }
}

pub fn compute(handler: &Handler) -> Result<WorstCase, Overflow> {
    let body = block(handler, &handler.body)?;
    let admission = [Cost::of(0), Cost::of(ADMISSION_RECORDS), Cost::of(0)];
    let [effects, records, requests] = combine(admission, body, None)?;
    Ok(WorstCase {
        effects,
        records,
        requests,
    })
}
