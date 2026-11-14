use crate::effect::EffectClass;
use crate::ir::{Bound, Code, Handler, Program, SiteKind, Stmt};

pub const ADMISSION_RECORDS: u64 = 1;
pub const FINISH_RECORDS: u64 = 2;
pub const EFFECT_RECORDS: u64 = 2;
pub const MODEL_RECORDS: u64 = 1;
pub const CELL_HEADER_RECORDS: u64 = 1;
pub const CELL_FINISH_RECORDS: u64 = 1;
pub const ENTRY_UPDATE_RECORDS: u64 = 1;
pub const NESTED_LOG_RECORDS: u64 = 1;
pub const NESTED_UPDATE_RECORDS: u64 = 2 * ENTRY_UPDATE_RECORDS;
pub const NESTED_EXTERNAL_RECORDS: u64 = 2 * NESTED_LOG_RECORDS;
pub const TIMER_UPDATE_RECORDS: u64 = 2 * ENTRY_UPDATE_RECORDS;

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

#[derive(Clone, Debug)]
struct Paths {
    through: Option<Costs>,
    ended: Option<Costs>,
}

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

    fn times(self, factor: &Factor, count: u64) -> Option<Self> {
        let total = self.total.checked_mul(count)?;
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

fn combine(left: Costs, right: Costs) -> Result<Costs, Overflow> {
    let mut out = zero();
    for (index, (left, right)) in left.into_iter().zip(right).enumerate() {
        out[index] = left.clone().then(right).ok_or_else(|| Overflow {
            measure: MEASURES[index],
            loops: left.loops.clone(),
        })?;
    }
    Ok(out)
}

fn repeated(costs: Costs, factor: &Factor, count: u64) -> Result<Costs, Overflow> {
    let mut out = zero();
    for (index, cost) in costs.into_iter().enumerate() {
        out[index] = cost.clone().times(factor, count).ok_or_else(|| {
            let mut loops = vec![factor.clone()];
            loops.extend(cost.loops.clone());
            Overflow {
                measure: MEASURES[index],
                loops,
            }
        })?;
    }
    Ok(out)
}

fn widest(left: Costs, right: Costs) -> Costs {
    let [a, b, c] = left;
    let [x, y, z] = right;
    [a.or(x), b.or(y), c.or(z)]
}

fn either(left: Option<Costs>, right: Option<Costs>) -> Option<Costs> {
    match (left, right) {
        (Some(left), Some(right)) => Some(widest(left, right)),
        (left, None) => left,
        (None, right) => right,
    }
}

fn continuing(costs: Costs) -> Paths {
    Paths {
        through: Some(costs),
        ended: None,
    }
}

fn branch(left: Paths, right: Paths) -> Paths {
    Paths {
        through: either(left.through, right.through),
        ended: either(left.ended, right.ended),
    }
}

fn sequence(first: Paths, second: Paths) -> Result<Paths, Overflow> {
    let Paths { through, ended } = first;
    let Some(before) = through else {
        return Ok(Paths {
            through: None,
            ended,
        });
    };
    let ended_after = match second.ended {
        Some(after) => Some(combine(before.clone(), after)?),
        None => None,
    };
    let through_after = match second.through {
        Some(after) => Some(combine(before, after)?),
        None => None,
    };
    Ok(Paths {
        through: through_after,
        ended: either(ended, ended_after),
    })
}

fn repeat(body: Paths, factor: &Factor, every_iteration: bool) -> Result<Paths, Overflow> {
    let count = factor.bound.value;
    if count == 0 {
        return Ok(continuing(zero()));
    }
    let through = match &body.through {
        Some(each) => Some(repeated(each.clone(), factor, count)?),
        None if every_iteration => None,
        None => Some(zero()),
    };
    let ended = match body.ended {
        Some(last) => {
            let before = match body.through {
                Some(each) => repeated(each, factor, count - 1)?,
                None => zero(),
            };
            Some(combine(before, last)?)
        }
        None => None,
    };
    Ok(Paths { through, ended })
}

fn overflow(measure: usize) -> Overflow {
    Overflow {
        measure: MEASURES[measure],
        loops: Vec::new(),
    }
}

fn cell_cost(program: &Program, code: &Code) -> Result<Costs, Overflow> {
    let calls = code.calls.value;
    let timers = code.timers.value;
    let effects = calls
        .checked_add(timers)
        .and_then(|nested| nested.checked_add(1))
        .ok_or_else(|| overflow(0))?;
    let per_call = if code.worst_class(program) == EffectClass::External {
        NESTED_UPDATE_RECORDS + NESTED_EXTERNAL_RECORDS
    } else {
        NESTED_UPDATE_RECORDS
    };
    let records = calls
        .checked_mul(per_call)
        .and_then(|nested| nested.checked_add(timers.checked_mul(TIMER_UPDATE_RECORDS)?))
        .and_then(|nested| {
            nested.checked_add(EFFECT_RECORDS + CELL_HEADER_RECORDS + CELL_FINISH_RECORDS)
        })
        .ok_or_else(|| overflow(1))?;
    Ok([Cost::of(effects), Cost::of(records), Cost::of(calls)])
}

fn site_cost(program: &Program, kind: SiteKind) -> Result<Costs, Overflow> {
    let plain = |records| {
        [
            Cost::of(1),
            Cost::of(records),
            Cost::of(u64::from(kind.outbound())),
        ]
    };
    match kind {
        SiteKind::Cell(code) => cell_cost(program, &program.codes[code]),
        SiteKind::Model => Ok(plain(MODEL_RECORDS)),
        SiteKind::Dispatch => {
            let mut widest_cost = plain(EFFECT_RECORDS);
            for code in &program.codes {
                widest_cost = widest(widest_cost, cell_cost(program, code)?);
            }
            Ok(widest_cost)
        }
        SiteKind::Tool(_) | SiteKind::Now | SiteKind::Random => Ok(plain(EFFECT_RECORDS)),
    }
}

fn block(program: &Program, handler: &Handler, statements: &[Stmt]) -> Result<Paths, Overflow> {
    let mut paths = continuing(zero());
    for statement in statements {
        let cost = statement_cost(program, handler, statement)?;
        paths = sequence(paths, cost)?;
    }
    Ok(paths)
}

fn statement_cost(
    program: &Program,
    handler: &Handler,
    statement: &Stmt,
) -> Result<Paths, Overflow> {
    match statement {
        Stmt::Let { .. } | Stmt::Assign { .. } | Stmt::Push { .. } => Ok(continuing(zero())),
        Stmt::Effect { site, .. } => Ok(continuing(site_cost(program, handler.sites[*site].kind)?)),
        Stmt::Finish { .. } => Ok(Paths {
            through: None,
            ended: Some([Cost::of(0), Cost::of(FINISH_RECORDS), Cost::of(0)]),
        }),
        Stmt::If {
            then, otherwise, ..
        } => Ok(branch(
            block(program, handler, then)?,
            block(program, handler, otherwise)?,
        )),
        Stmt::Match { arms, .. } => {
            let mut widest_arm = Paths {
                through: None,
                ended: None,
            };
            for arm in arms {
                widest_arm = branch(widest_arm, block(program, handler, &arm.body)?);
            }
            Ok(widest_arm)
        }
        Stmt::Loop { index, bound, body } => {
            let factor = Factor {
                name: handler.locals[*index].name.clone(),
                bound: bound.clone(),
            };
            repeat(block(program, handler, body)?, &factor, true)
        }
        Stmt::For {
            item, bound, body, ..
        } => {
            let factor = Factor {
                name: handler.locals[*item].name.clone(),
                bound: bound.clone(),
            };
            repeat(block(program, handler, body)?, &factor, false)
        }
    }
}

pub fn compute(program: &Program, handler: &Handler) -> Result<WorstCase, Overflow> {
    let body = block(program, handler, &handler.body)?;
    let longest =
        either(body.through, body.ended).expect("a block keeps a path through it or ending in it");
    let admission = [Cost::of(0), Cost::of(ADMISSION_RECORDS), Cost::of(0)];
    let [effects, records, requests] = combine(admission, longest)?;
    Ok(WorstCase {
        effects,
        records,
        requests,
    })
}
