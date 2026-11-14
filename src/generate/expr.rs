use crate::effect::EffectClass;
use crate::generate::{CALL, Decls, ENTRY, REPLY, Ty, chance, weighted};
use crate::sim::rng::Rng;

const TEXT_LITERALS: [&str; 17] = [
    "constructor",
    "toString",
    "__proto__",
    "",
    "a",
    "hello world",
    "q\"uote",
    "back\\slash",
    "line\nbreak",
    "tab\there",
    "cr\rhere",
    "\u{1}",
    "\u{e9}\u{2713}",
    "\u{1f600}",
    "\u{2028}",
    "{}",
    "ok",
];
const SMALL_NUMBER_MAX: u64 = 9;
const EDGE_NUMBERS: [u64; 5] = [u64::MAX, u64::MAX - 1, 1 << 32, (1 << 32) - 1, 1 << 63];
const EDGE_NUMBER_PERCENT: u64 = 10;
const TEXT_PARTS_MAX: u64 = 3;
const TEXT_ATOM_PERCENT: u64 = 60;
const UNIT_PARENS_PERCENT: u64 = 30;
const TOOL_NAME_PERCENT: u64 = 20;
const PARENS_PERCENT: u64 = 20;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    Literal,
    Atom,
    Binary,
    Len,
    Now,
    Random,
    Compare,
    Equal,
    Logic,
    Not,
    Construct,
    Effect,
}

const U64_PICKS: [(Pick, u64); 6] = [
    (Pick::Literal, 4),
    (Pick::Atom, 5),
    (Pick::Binary, 3),
    (Pick::Len, 2),
    (Pick::Now, 1),
    (Pick::Random, 1),
];
const BOOL_PICKS: [(Pick, u64); 6] = [
    (Pick::Literal, 2),
    (Pick::Atom, 3),
    (Pick::Compare, 4),
    (Pick::Equal, 3),
    (Pick::Logic, 2),
    (Pick::Not, 1),
];
const SUM_PICKS: [(Pick, u64); 3] = [(Pick::Construct, 3), (Pick::Atom, 3), (Pick::Effect, 6)];
const COMPARISONS: [&str; 4] = ["<", "<=", ">", ">="];
const ARITHMETIC: [&str; 3] = ["+", "-", "*"];
const ARITHMETIC_WEIGHTS: [(usize, u64); 3] = [(0, 4), (1, 3), (2, 1)];

pub struct Context<'a> {
    pub decls: &'a Decls,
    pub locals: Vec<(String, Ty)>,
    pub indices: Vec<String>,
    pub effects: bool,
    pub fields: bool,
}

pub fn text_literal(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn parens(rng: &mut Rng, text: String) -> String {
    if chance(rng, PARENS_PERCENT) {
        format!("({text})")
    } else {
        text
    }
}

impl Context<'_> {
    pub fn atoms(&self, ty: &Ty) -> Vec<String> {
        let mut atoms: Vec<String> = self
            .locals
            .iter()
            .filter(|(_, local)| local == ty)
            .map(|(name, _)| name.clone())
            .collect();
        if self.fields {
            atoms.extend(
                self.decls
                    .fields
                    .iter()
                    .filter(|(_, field)| field == ty)
                    .map(|(name, _)| name.clone()),
            );
        }
        if *ty == Ty::U64 {
            atoms.extend(self.decls.constants.iter().map(|(name, _)| name.clone()));
        }
        atoms
    }

    fn atom(&self, rng: &mut Rng, ty: &Ty) -> Option<String> {
        let atoms = self.atoms(ty);
        (!atoms.is_empty()).then(|| atoms[rng.pick(atoms.len())].clone())
    }

    pub fn lists(&self) -> Vec<(String, Ty)> {
        let mut lists: Vec<(String, Ty)> = self
            .locals
            .iter()
            .filter(|(_, ty)| matches!(ty, Ty::List(..)))
            .cloned()
            .collect();
        if self.fields {
            lists.extend(
                self.decls
                    .fields
                    .iter()
                    .filter(|(_, ty)| matches!(ty, Ty::List(..)))
                    .cloned(),
            );
        }
        lists
    }

    pub fn expr(&mut self, rng: &mut Rng, ty: &Ty, expected: bool, depth: u32) -> Option<String> {
        match ty {
            Ty::U64 | Ty::Bool | Ty::Text => Some(self.scalar(rng, ty, depth)),
            Ty::List(..) => self.atom(rng, ty),
            Ty::Sum(_) | Ty::Known(_) | Ty::Maybe(_) => self.sum(rng, ty, expected, depth),
        }
    }

    fn feasible(&self, table: &[(Pick, u64)], depth: u32) -> Vec<(Pick, u64)> {
        let has_lists = !self.lists().is_empty();
        let has = |ty: &Ty| !self.atoms(ty).is_empty();
        table
            .iter()
            .copied()
            .filter(|(pick, _)| match pick {
                Pick::Atom => has(if table == U64_PICKS {
                    &Ty::U64
                } else {
                    &Ty::Bool
                }),
                Pick::Binary | Pick::Logic | Pick::Not => depth > 0,
                Pick::Len => has_lists,
                Pick::Now | Pick::Random => self.effects,
                _ => true,
            })
            .collect()
    }

    pub fn number(&mut self, rng: &mut Rng, depth: u32) -> String {
        let picks = self.feasible(&U64_PICKS, depth);
        match weighted(rng, &picks) {
            Pick::Atom => self.atom(rng, &Ty::U64).expect("a feasible atom exists"),
            Pick::Binary if self.decls.provable => self.bounded_sum(rng),
            Pick::Binary => {
                let op = ARITHMETIC[weighted(rng, &ARITHMETIC_WEIGHTS)];
                let left = self.number(rng, depth - 1);
                let right = self.number(rng, depth - 1);
                parens(rng, format!("{left} {op} {right}"))
            }
            Pick::Len => {
                let lists = self.lists();
                format!("len({})", lists[rng.pick(lists.len())].0)
            }
            Pick::Now => "now()".to_owned(),
            Pick::Random => "random()".to_owned(),
            _ => self.number_literal(rng),
        }
    }

    fn bounded_sum(&mut self, rng: &mut Rng) -> String {
        let small = rng.below(SMALL_NUMBER_MAX + 1);
        let lists = self.lists();
        let index = !self.indices.is_empty() && (lists.is_empty() || rng.below(2) == 0);
        let base = if index {
            self.indices[rng.pick(self.indices.len())].clone()
        } else if !lists.is_empty() {
            format!("len({})", lists[rng.pick(lists.len())].0)
        } else {
            return small.to_string();
        };
        format!("{base} + {small}")
    }

    fn number_literal(&self, rng: &mut Rng) -> String {
        if chance(rng, EDGE_NUMBER_PERCENT) && !self.decls.provable {
            return EDGE_NUMBERS[rng.pick(EDGE_NUMBERS.len())].to_string();
        }
        rng.below(SMALL_NUMBER_MAX + 1).to_string()
    }

    fn scalar(&mut self, rng: &mut Rng, ty: &Ty, depth: u32) -> String {
        match ty {
            Ty::U64 => self.number(rng, depth),
            Ty::Bool => self.boolean(rng, depth),
            Ty::Text => self.text(rng),
            other => panic!("a scalar is u64, bool or text, found {other:?}"),
        }
    }

    pub fn boolean(&mut self, rng: &mut Rng, depth: u32) -> String {
        let picks = self.feasible(&BOOL_PICKS, depth);
        let next = depth.saturating_sub(1);
        match weighted(rng, &picks) {
            Pick::Atom => self.atom(rng, &Ty::Bool).expect("a feasible atom exists"),
            Pick::Compare => {
                let op = COMPARISONS[rng.pick(COMPARISONS.len())];
                let left = self.number(rng, next);
                let right = self.number(rng, next);
                format!("{left} {op} {right}")
            }
            Pick::Equal => {
                let ty = crate::generate::decls::scalar(rng);
                let op = ["==", "!="][rng.pick(2)];
                let left = self.scalar(rng, &ty, next);
                let right = self.scalar(rng, &ty, next);
                if ty == Ty::Bool {
                    return format!("({left}) {op} ({right})");
                }
                format!("{} {op} {}", parens(rng, left), parens(rng, right))
            }
            Pick::Logic => {
                let op = ["and", "or"][rng.pick(2)];
                let left = self.boolean(rng, next);
                let right = self.boolean(rng, next);
                format!("({left}) {op} ({right})")
            }
            Pick::Not => format!("not ({})", self.boolean(rng, next)),
            _ => ["true", "false"][rng.pick(2)].to_owned(),
        }
    }

    fn text_atoms(&self) -> Vec<String> {
        let mut atoms = self.atoms(&Ty::Text);
        for call in self.atoms(&Ty::Sum(CALL)) {
            for member in crate::ir::CALL_FIELDS {
                atoms.push(format!("{call}.{member}"));
            }
        }
        atoms
    }

    fn text(&mut self, rng: &mut Rng) -> String {
        let parts = rng.range(1, TEXT_PARTS_MAX);
        let atoms = self.text_atoms();
        let atom_at =
            (chance(rng, TEXT_ATOM_PERCENT) && !atoms.is_empty()).then(|| rng.below(parts));
        let mut pieces = Vec::new();
        for index in 0..parts {
            if Some(index) == atom_at {
                pieces.push(atoms[rng.pick(atoms.len())].clone());
            } else if chance(rng, TOOL_NAME_PERCENT) && !self.decls.tools.is_empty() {
                let tool = &self.decls.tools[rng.pick(self.decls.tools.len())];
                pieces.push(text_literal(&tool.name));
            } else {
                pieces.push(text_literal(TEXT_LITERALS[rng.pick(TEXT_LITERALS.len())]));
            }
        }
        pieces.join(" ++ ")
    }

    fn sum(&mut self, rng: &mut Rng, ty: &Ty, expected: bool, depth: u32) -> Option<String> {
        let mut picks = SUM_PICKS.to_vec();
        while !picks.is_empty() {
            let pick = weighted(rng, &picks);
            picks.retain(|(other, _)| *other != pick);
            let made = match pick {
                Pick::Construct => self.construct(rng, ty, expected, depth),
                Pick::Atom => self.atom(rng, ty),
                _ => self.effect(rng, ty, depth),
            };
            if made.is_some() {
                return made;
            }
        }
        None
    }

    fn construct(&mut self, rng: &mut Rng, ty: &Ty, expected: bool, depth: u32) -> Option<String> {
        let generic = matches!(ty, Ty::Known(_) | Ty::Maybe(_)) || *ty == Ty::Sum(ENTRY);
        let mut variants = self.decls.variants(ty).expect("a sum type has variants");
        crate::generate::decls::shuffle(rng, &mut variants);
        for (name, fields) in variants {
            let ambiguous = crate::ir::OUTCOME_VARIANTS.contains(&name.as_str());
            if ambiguous && generic && !expected {
                continue;
            }
            if let Some(made) = self.variant(rng, &name, &fields, depth) {
                return Some(made);
            }
        }
        None
    }

    fn variant(&mut self, rng: &mut Rng, name: &str, fields: &[Ty], depth: u32) -> Option<String> {
        if fields.is_empty() {
            let parens = chance(rng, UNIT_PARENS_PERCENT);
            return Some(if parens {
                format!("{name}()")
            } else {
                name.to_owned()
            });
        }
        let mut args = Vec::new();
        for field in fields {
            args.push(self.expr(rng, field, true, depth.saturating_sub(1))?);
        }
        Some(format!("{name}({})", args.join(", ")))
    }

    fn effect(&mut self, rng: &mut Rng, ty: &Ty, depth: u32) -> Option<String> {
        if !self.effects || depth == 0 {
            return None;
        }
        if *ty == Ty::Sum(REPLY) {
            let model = self.decls.model.clone()?;
            let contexts = self.atoms_matching(
                |ty| matches!(ty, Ty::List(element, _) if **element == Ty::Sum(ENTRY)),
            );
            if contexts.is_empty() {
                return None;
            }
            let context = contexts[rng.pick(contexts.len())].clone();
            return Some(format!("{model}({context})"));
        }
        let (inner, external) = match ty {
            Ty::Known(inner) => (inner, false),
            Ty::Maybe(inner) => (inner, true),
            _ => return None,
        };
        let mut candidates: Vec<Option<usize>> = (0..self.decls.tools.len())
            .filter(|index| {
                let tool = &self.decls.tools[*index];
                tool.returns == **inner && (tool.effect == EffectClass::External) == external
            })
            .map(Some)
            .collect();
        if external && **inner == Ty::Text {
            candidates.push(None);
        }
        if candidates.is_empty() {
            return None;
        }
        let Some(choice) = candidates[rng.pick(candidates.len())] else {
            let call = self.expr(rng, &Ty::Sum(CALL), true, depth - 1)?;
            return Some(format!("dispatch({call})"));
        };
        let tool = self.decls.tools[choice].clone();
        let mut args = Vec::new();
        for param in &tool.params {
            args.push(self.expr(rng, param, true, depth - 1)?);
        }
        Some(format!("{}({})", tool.name, args.join(", ")))
    }

    pub fn constructible(&self, ty: &Ty) -> bool {
        match ty {
            Ty::U64 | Ty::Bool | Ty::Text | Ty::Known(_) | Ty::Maybe(_) => true,
            Ty::List(..) => !self.atoms(ty).is_empty(),
            Ty::Sum(_) => self
                .decls
                .variants(ty)
                .expect("a sum has variants")
                .iter()
                .any(|(_, fields)| fields.iter().all(|field| self.constructible(field))),
        }
    }

    pub fn atoms_matching(&self, wanted: impl Fn(&Ty) -> bool) -> Vec<String> {
        let mut atoms: Vec<String> = self
            .locals
            .iter()
            .filter(|(_, ty)| wanted(ty))
            .map(|(name, _)| name.clone())
            .collect();
        if self.fields {
            atoms.extend(
                self.decls
                    .fields
                    .iter()
                    .filter(|(_, ty)| wanted(ty))
                    .map(|(name, _)| name.clone()),
            );
        }
        atoms
    }
}
