mod body;
pub mod cli;
mod decls;
mod expr;
pub mod mutate;
mod names;
pub mod sound;

use crate::effect::EffectClass;
use crate::sim::rng::Rng;

pub const ATTEMPTS_MAX: u64 = 64;
pub const EFFECTS_PER_HANDLER_MAX: u64 = 256;
pub const PERCENT: u64 = 100;
pub const CALL: usize = 0;
pub const REPLY: usize = 1;
pub const ENTRY: usize = 2;
pub const BUILTIN_SUMS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    U64,
    Bool,
    Text,
    Sum(usize),
    List(Box<Ty>, usize),
    Known(Box<Ty>),
    Maybe(Box<Ty>),
}

#[derive(Clone, Debug)]
pub struct SumDef {
    pub name: String,
    pub variants: Vec<(String, Vec<Ty>)>,
}

#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub params: Vec<Ty>,
    pub returns: Ty,
    pub effect: EffectClass,
}

#[derive(Clone, Debug, Default)]
pub struct Decls {
    pub constants: Vec<(String, u64)>,
    pub loop_bounds: Vec<usize>,
    pub capacities: Vec<usize>,
    pub calls_bound: Option<usize>,
    pub sums: Vec<SumDef>,
    pub tools: Vec<ToolDef>,
    pub model: Option<String>,
    pub fields: Vec<(String, Ty)>,
    pub names: std::collections::BTreeSet<String>,
    pub provable: bool,
    pub wait_ms: Option<usize>,
    pub dispatch_hooks: bool,
    pub answering: bool,
    pub cut_tokens: Option<usize>,
    pub defaults: Vec<Vec<Option<String>>>,
}

impl Decls {
    pub fn render_type(&self, ty: &Ty) -> String {
        match ty {
            Ty::U64 => "u64".to_owned(),
            Ty::Bool => "bool".to_owned(),
            Ty::Text => "text".to_owned(),
            Ty::Sum(sum) => self.sums[*sum].name.clone(),
            Ty::List(element, capacity) => format!(
                "list<{}, {}>",
                self.render_type(element),
                self.constants[*capacity].0
            ),
            Ty::Known(inner) => format!("known<{}>", self.render_type(inner)),
            Ty::Maybe(inner) => format!("maybe<{}>", self.render_type(inner)),
        }
    }

    pub fn variants(&self, ty: &Ty) -> Option<Vec<(String, Vec<Ty>)>> {
        let outcome = |inner: &Ty, unknown: bool| {
            let mut variants = vec![
                ("ok".to_owned(), vec![inner.clone()]),
                ("failed".to_owned(), vec![Ty::Text]),
            ];
            if unknown {
                variants.push(("unknown".to_owned(), Vec::new()));
            }
            variants
        };
        match ty {
            Ty::Sum(sum) => Some(self.sums[*sum].variants.clone()),
            Ty::Known(inner) => Some(outcome(inner, false)),
            Ty::Maybe(inner) => Some(outcome(inner, true)),
            Ty::U64 | Ty::Bool | Ty::Text | Ty::List(..) => None,
        }
    }

    pub fn has_model(&self) -> bool {
        self.model.is_some()
    }
}

pub fn chance(rng: &mut Rng, percent: u64) -> bool {
    assert!(percent <= PERCENT, "a chance is a percentage");
    rng.below(PERCENT) < percent
}

pub fn weighted<T: Copy>(rng: &mut Rng, table: &[(T, u64)]) -> T {
    let total: u64 = table.iter().map(|(_, weight)| weight).sum();
    assert!(total > 0, "a weighted choice has some weight");
    let mut drawn = rng.below(total);
    for (choice, weight) in table {
        if drawn < *weight {
            return *choice;
        }
        drawn -= weight;
    }
    unreachable!("a draw below the total weight lands on a choice")
}

pub struct Generated {
    pub source: String,
    pub attempt: u64,
    pub effects_max: u64,
    pub provable: bool,
}

pub fn program(seed: u64) -> Result<Generated, String> {
    for attempt in 0..ATTEMPTS_MAX {
        let mut rng = Rng::derive_keyed(seed, "generate", &[attempt]);
        let (source, provable) = render(&mut rng, seed);
        let parsed = crate::syntax::parse(&source).unwrap_or_else(|diagnostic| {
            panic!(
                "a generated program parses: {}\n{source}",
                diagnostic.render("generated.hl")
            )
        });
        let checked = crate::check::check(&parsed).unwrap_or_else(|diagnostic| {
            panic!(
                "a generated program checks: {}\n{source}",
                diagnostic.render("generated.hl")
            )
        });
        let effects_max = checked
            .handlers
            .iter()
            .map(|handler| handler.worst.effects.total)
            .max()
            .expect("a checked program has a handler");
        if effects_max <= EFFECTS_PER_HANDLER_MAX {
            return Ok(Generated {
                source,
                attempt,
                effects_max,
                provable,
            });
        }
    }
    Err(format!(
        "seed {seed}: no program within effects_per_handler_max ({EFFECTS_PER_HANDLER_MAX}) in attempts_max ({ATTEMPTS_MAX}) attempts"
    ))
}

fn render(rng: &mut Rng, seed: u64) -> (String, bool) {
    let decls = decls::generate(rng);
    let mut lines = vec![format!("harness fuzz{seed}"), String::new()];
    lines.extend(decls::render(rng, &decls));
    lines.extend(body::invariants(rng, &decls));
    lines.extend(body::hooks(rng, &decls));
    lines.extend(body::dispatch_hooks(rng, &decls));
    let handlers = rng.range(1, decls::HANDLERS_MAX);
    let mut names = std::collections::BTreeSet::new();
    for index in 0..handlers {
        lines.push(String::new());
        lines.extend(body::handler(rng, &decls, index, &mut names));
    }
    let mut source = lines.join("\n");
    source.push('\n');
    (source, decls.provable)
}
