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
    pub wait_ms: Cost,
    pub blob_bytes: Cost,
    pub spawns: Cost,
    pub family: Option<Family>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Family {
    pub child: [u64; 3],
    pub total: [u64; 3],
    pub sessions: u64,
    pub depth: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tree {
    pub measures: [u64; 3],
    pub sessions: u64,
    pub depth: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Levels {
    Tree,
    One,
}

pub fn family_with(
    program: &Program,
    own: [u64; 3],
    spawns: &Cost,
    levels: Levels,
) -> Result<Option<Family>, Overflow> {
    if spawns.total == 0 {
        return Ok(None);
    }
    let mut child = [0; 3];
    let mut sessions = 0;
    let mut depth = 0;
    for (_, spawn) in program.spawns() {
        let tree = match levels {
            Levels::Tree => spawn.tree(),
            Levels::One => {
                let worst = &spawn.child().worst;
                Tree {
                    measures: [
                        worst.effects.total,
                        worst.records.total,
                        worst.requests.total,
                    ],
                    sessions: 1,
                    depth: 0,
                }
            }
        };
        for (widest, value) in child.iter_mut().zip(tree.measures) {
            *widest = (*widest).max(value);
        }
        sessions = sessions.max(tree.sessions);
        depth = depth.max(tree.depth);
    }
    if program.forks() {
        sessions = sessions.max(1);
    }
    let mut total = [0; 3];
    for index in 0..3 {
        total[index] = spawns
            .total
            .checked_mul(child[index])
            .and_then(|children| children.checked_add(own[index]))
            .ok_or_else(|| Overflow {
                measure: MEASURES[index],
                loops: spawns.loops.clone(),
            })?;
    }
    let sessions = spawns.total.checked_mul(sessions).ok_or_else(|| Overflow {
        measure: MEASURES[5],
        loops: spawns.loops.clone(),
    })?;
    Ok(Some(Family {
        child,
        total,
        sessions,
        depth: depth + 1,
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overflow {
    pub measure: &'static str,
    pub loops: Vec<Factor>,
}

pub const MEASURES: [&str; 6] = [
    "effects",
    "journal records",
    "outbound requests",
    "milliseconds of waits",
    "blob bytes",
    "child sessions",
];

type Costs = [Cost; MEASURES.len()];

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
    std::array::from_fn(|_| Cost::default())
}

fn costs(effects: u64, records: u64, requests: u64, wait_ms: u64) -> Costs {
    [
        Cost::of(effects),
        Cost::of(records),
        Cost::of(requests),
        Cost::of(wait_ms),
        Cost::default(),
        Cost::default(),
    ]
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
    let mut right = right.into_iter();
    left.map(|cost| cost.or(right.next().expect("both sides hold every measure")))
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
    Ok(costs(effects, records, calls, 0))
}

fn blob_bytes(program: &Program, kind: SiteKind) -> u64 {
    match kind {
        SiteKind::Tool(tool) => match &program.tools[tool].returns {
            crate::ir::Type::Blob(bytes) => bytes.value,
            _ => 0,
        },
        SiteKind::Dispatch => program.blobs.as_ref().map_or(0, |blobs| blobs.bytes.value),
        _ => 0,
    }
}

fn site_cost(program: &Program, kind: SiteKind) -> Result<Costs, Overflow> {
    let spawns = match kind {
        SiteKind::Tool(tool) => program.starts_sessions(&program.tools[tool]),
        SiteKind::Dispatch => program.spawns().next().is_some() || program.forks(),
        _ => false,
    };
    let plain = |records| {
        let mut plain = costs(1, records, u64::from(kind.outbound()), 0);
        plain[4] = Cost::of(blob_bytes(program, kind));
        plain[5] = Cost::of(u64::from(spawns));
        plain
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
        SiteKind::Tool(_)
        | SiteKind::Listing(_)
        | SiteKind::Now
        | SiteKind::Random
        | SiteKind::Suspend
        | SiteKind::Deny => Ok(plain(EFFECT_RECORDS)),
        SiteKind::Wait(constant) | SiteKind::Hold(constant) => {
            let mut waited = plain(EFFECT_RECORDS);
            waited[3] = Cost::of(program.constants[constant].value);
            Ok(waited)
        }
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
        Stmt::Let { .. }
        | Stmt::Assign { .. }
        | Stmt::Push { .. }
        | Stmt::Take { .. }
        | Stmt::KeepBlobs { .. }
        | Stmt::Rewrite { .. }
        | Stmt::Shrink { .. }
        | Stmt::Set { .. }
        | Stmt::SetIndex { .. }
        | Stmt::PushLocal { .. } => Ok(continuing(zero())),
        Stmt::Effect { site, .. } => Ok(continuing(site_cost(program, handler.sites[*site].kind)?)),
        Stmt::Finish { .. } => Ok(Paths {
            through: None,
            ended: Some(costs(0, FINISH_RECORDS, 0, 0)),
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
        Stmt::Loop { index, bound, body }
        | Stmt::While {
            index, bound, body, ..
        } => {
            let factor = Factor {
                name: handler.locals[*index].name.clone(),
                bound: bound.clone(),
            };
            repeat(block(program, handler, body)?, &factor, true)
        }
        Stmt::Parallel(parallel) => statement_cost(program, handler, &parallel.sequential()),
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
    let admission = costs(0, ADMISSION_RECORDS, 0, 0);
    let [effects, records, requests, wait_ms, blob_bytes, spawns] = combine(admission, longest)?;
    let family = family_with(
        program,
        [effects.total, records.total, requests.total],
        &spawns,
        Levels::Tree,
    )?;
    Ok(WorstCase {
        effects,
        records,
        requests,
        wait_ms,
        blob_bytes,
        spawns,
        family,
    })
}
