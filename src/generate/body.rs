use crate::generate::decls::{field_type, shuffle};
use crate::generate::expr::Context;
use crate::generate::names::fresh;
use crate::generate::{CALL, Decls, ENTRY, REPLY, Ty, chance, weighted};
use crate::sim::rng::Rng;

const STATEMENTS_PER_HANDLER_MAX: u64 = 40;
const STATEMENTS_PER_BLOCK_MAX: u64 = 6;
const LET_EFFECT_PERCENT: u64 = 70;
const BLOCK_DEPTH_MAX: u32 = 4;
const LOOP_NESTING_MAX: u32 = 2;
const EXPRESSION_DEPTH_MAX: u32 = 2;
const PARAMS_PER_HANDLER_MAX: u64 = 3;
const INVARIANTS_MAX: u64 = 2;
const REQUIRES_PERCENT: u64 = 40;
const ELSE_PERCENT: u64 = 50;
const ELSE_IF_PERCENT: u64 = 30;
const WILDCARD_PERCENT: u64 = 20;
const ARM_COMMA_PERCENT: u64 = 30;
const INDENT: &str = "  ";
const RETURN_TYPE_DRAWS_MAX: u64 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Let,
    Assign,
    Push,
    If,
    Match,
    Loop,
    For,
    Return,
}

const STATEMENT_WEIGHTS: [(Kind, u64); 8] = [
    (Kind::Let, 5),
    (Kind::Assign, 4),
    (Kind::Push, 4),
    (Kind::If, 3),
    (Kind::Match, 5),
    (Kind::Loop, 2),
    (Kind::For, 2),
    (Kind::Return, 1),
];

struct Body<'a> {
    context: Context<'a>,
    used: std::collections::BTreeSet<String>,
    names: u64,
    statements_left: u64,
    loops: u32,
    returns: Ty,
    lines: Vec<String>,
}

pub fn invariants(rng: &mut Rng, decls: &Decls) -> Vec<String> {
    let mut context = Context {
        decls,
        locals: Vec::new(),
        effects: false,
        fields: true,
    };
    (0..rng.below(INVARIANTS_MAX + 1))
        .map(|index| {
            let condition = context.boolean(rng, EXPRESSION_DEPTH_MAX);
            format!("invariant inv{index}: {condition}")
        })
        .collect()
}

fn param_type(rng: &mut Rng, decls: &Decls) -> Ty {
    field_type(rng, decls, decls.sums.len())
}

fn return_type(rng: &mut Rng, decls: &Decls) -> Ty {
    let context = Context {
        decls,
        locals: Vec::new(),
        effects: false,
        fields: true,
    };
    for _ in 0..RETURN_TYPE_DRAWS_MAX {
        let ty = param_type(rng, decls);
        if context.constructible(&ty) {
            return ty;
        }
    }
    Ty::Text
}

pub fn handler(
    rng: &mut Rng,
    decls: &Decls,
    index: u64,
    handlers: &mut std::collections::BTreeSet<String>,
) -> Vec<String> {
    let mut used = decls.names.clone();
    used.extend(handlers.iter().cloned());
    let name = fresh(rng, &mut used, format!("h{index}"));
    handlers.insert(name.clone());
    let params: Vec<(String, Ty)> = (0..rng.below(PARAMS_PER_HANDLER_MAX + 1))
        .map(|param| {
            let ty = param_type(rng, decls);
            (fresh(rng, &mut used, format!("p{param}")), ty)
        })
        .collect();
    let returns = return_type(rng, decls);
    let rendered: Vec<String> = params
        .iter()
        .map(|(name, ty)| format!("{name}: {}", decls.render_type(ty)))
        .collect();
    let mut body = Body {
        context: Context {
            decls,
            locals: params,
            effects: false,
            fields: true,
        },
        used,
        names: 0,
        statements_left: STATEMENTS_PER_HANDLER_MAX,
        loops: 0,
        returns: returns.clone(),
        lines: Vec::new(),
    };
    body.lines.push(format!(
        "on {name}({}) -> {}",
        rendered.join(", "),
        decls.render_type(&returns)
    ));
    if chance(rng, REQUIRES_PERCENT) {
        let condition = body.context.boolean(rng, EXPRESSION_DEPTH_MAX);
        body.lines.push(format!("{INDENT}requires {condition}"));
    }
    body.context.effects = true;
    body.lines.push("{".to_owned());
    let returned = body.statements(rng, 1);
    if !returned {
        body.final_return(rng);
    }
    body.lines.push("}".to_owned());
    body.lines
}

impl Body<'_> {
    fn line(&mut self, depth: u32, text: String) {
        self.lines
            .push(format!("{}{text}", INDENT.repeat(depth as usize)));
    }

    fn name(&mut self, rng: &mut Rng, prefix: &str) -> String {
        self.names += 1;
        fresh(rng, &mut self.used, format!("{prefix}{}", self.names))
    }

    fn final_return(&mut self, rng: &mut Rng) {
        let ty = self.returns.clone();
        let value = self
            .context
            .expr(rng, &ty, true, EXPRESSION_DEPTH_MAX)
            .expect("every return type has a constructible value");
        self.line(1, format!("return {value}"));
    }

    fn statements(&mut self, rng: &mut Rng, depth: u32) -> bool {
        let scope = self.context.locals.len();
        let count = rng.range(1, STATEMENTS_PER_BLOCK_MAX);
        let mut returned = false;
        for _ in 0..count {
            if self.statements_left == 0 || returned {
                break;
            }
            self.statements_left -= 1;
            returned = self.statement(rng, depth);
        }
        self.context.locals.truncate(scope);
        returned
    }

    fn block(&mut self, rng: &mut Rng, depth: u32) -> bool {
        self.statements(rng, depth)
    }

    fn statement(&mut self, rng: &mut Rng, depth: u32) -> bool {
        let nested = depth < BLOCK_DEPTH_MAX;
        let looping = nested && self.loops < LOOP_NESTING_MAX;
        let picks: Vec<(Kind, u64)> = STATEMENT_WEIGHTS
            .iter()
            .copied()
            .filter(|(kind, _)| match kind {
                Kind::If | Kind::Match => nested,
                Kind::Loop | Kind::For => looping,
                Kind::Return => depth > 1,
                _ => true,
            })
            .collect();
        match weighted(rng, &picks) {
            Kind::Let => self.let_statement(rng, depth),
            Kind::Assign => self.assign(rng, depth),
            Kind::Push => self.push(rng, depth),
            Kind::If => return self.if_statement(rng, depth, "if"),
            Kind::Match => return self.match_statement(rng, depth),
            Kind::Loop => self.loop_statement(rng, depth),
            Kind::For => self.for_statement(rng, depth),
            Kind::Return => {
                let ty = self.returns.clone();
                if let Some(value) = self.context.expr(rng, &ty, true, EXPRESSION_DEPTH_MAX) {
                    self.line(depth, format!("return {value}"));
                    return true;
                }
            }
        }
        false
    }

    fn let_statement(&mut self, rng: &mut Rng, depth: u32) {
        let candidates = self.subject_types(rng);
        let ty = if candidates.is_empty() || !chance(rng, LET_EFFECT_PERCENT) {
            param_type(rng, self.context.decls)
        } else {
            candidates[rng.pick(candidates.len())].clone()
        };
        let Some(value) = self.context.expr(rng, &ty, false, EXPRESSION_DEPTH_MAX) else {
            return;
        };
        let name = self.name(rng, "v");
        self.line(depth, format!("let {name} = {value}"));
        self.context.locals.push((name, ty));
    }

    fn assign(&mut self, rng: &mut Rng, depth: u32) {
        let fields: Vec<(String, Ty)> = self
            .context
            .decls
            .fields
            .iter()
            .filter(|(_, ty)| !matches!(ty, Ty::List(..)))
            .cloned()
            .collect();
        if fields.is_empty() {
            return;
        }
        let (name, ty) = fields[rng.pick(fields.len())].clone();
        if let Some(value) = self.context.expr(rng, &ty, true, EXPRESSION_DEPTH_MAX) {
            self.line(depth, format!("{name} = {value}"));
        }
    }

    fn push(&mut self, rng: &mut Rng, depth: u32) {
        let lists: Vec<(String, Ty)> = self
            .context
            .decls
            .fields
            .iter()
            .filter(|(_, ty)| matches!(ty, Ty::List(..)))
            .cloned()
            .collect();
        if lists.is_empty() {
            return;
        }
        let (name, ty) = lists[rng.pick(lists.len())].clone();
        let Ty::List(element, _) = ty else {
            unreachable!("filtered to lists")
        };
        if let Some(value) = self.context.expr(rng, &element, true, EXPRESSION_DEPTH_MAX) {
            self.line(depth, format!("push {name} {value}"));
        }
    }

    fn if_statement(&mut self, rng: &mut Rng, depth: u32, keyword: &str) -> bool {
        let condition = self.context.boolean(rng, EXPRESSION_DEPTH_MAX);
        if keyword == "if" {
            self.line(depth, format!("if {condition} {{"));
        } else {
            let last = self.lines.last_mut().expect("an else follows a block");
            last.push_str(&format!(" if {condition} {{"));
        }
        let then = self.block(rng, depth + 1);
        if !chance(rng, ELSE_PERCENT) {
            self.line(depth, "}".to_owned());
            return false;
        }
        self.line(depth, "} else".to_owned());
        if chance(rng, ELSE_IF_PERCENT) {
            return self.if_statement(rng, depth, "else") && then;
        }
        self.lines.last_mut().expect("just pushed").push_str(" {");
        let otherwise = self.block(rng, depth + 1);
        self.line(depth, "}".to_owned());
        then && otherwise
    }

    fn subject_types(&self, rng: &mut Rng) -> Vec<Ty> {
        let decls = self.context.decls;
        let mut types: Vec<Ty> = Vec::new();
        for tool in &decls.tools {
            let inner = Box::new(tool.returns.clone());
            types.push(if tool.effect.may_be_unknown() {
                Ty::Maybe(inner)
            } else {
                Ty::Known(inner)
            });
        }
        types.push(Ty::Maybe(Box::new(Ty::Text)));
        if decls.has_model() {
            types.push(Ty::Sum(REPLY));
            types.push(Ty::Sum(ENTRY));
        }
        types.push(Ty::Sum(CALL));
        for sum in crate::generate::BUILTIN_SUMS..decls.sums.len() {
            types.push(Ty::Sum(sum));
        }
        for (_, ty) in &self.context.locals {
            if decls.variants(ty).is_some() {
                types.push(ty.clone());
            }
        }
        shuffle(rng, &mut types);
        types
    }

    fn match_statement(&mut self, rng: &mut Rng, depth: u32) -> bool {
        let candidates = self.subject_types(rng);
        let found = candidates.into_iter().find_map(|ty| {
            let subject = self.context.expr(rng, &ty, false, EXPRESSION_DEPTH_MAX)?;
            Some((ty, subject))
        });
        let Some((ty, subject)) = found else {
            return false;
        };
        self.line(depth, format!("match {subject} {{"));
        let mut variants = self
            .context
            .decls
            .variants(&ty)
            .expect("a subject is a sum");
        shuffle(rng, &mut variants);
        let mut all_return = true;
        for (variant, fields) in variants {
            all_return &= self.arm(rng, depth + 1, &variant, &fields);
        }
        self.line(depth, "}".to_owned());
        all_return
    }

    fn arm(&mut self, rng: &mut Rng, depth: u32, variant: &str, fields: &[Ty]) -> bool {
        let scope = self.context.locals.len();
        let mut bindings = Vec::new();
        for field in fields {
            if chance(rng, WILDCARD_PERCENT) {
                bindings.push("_".to_owned());
                continue;
            }
            let name = self.name(rng, "b");
            bindings.push(name.clone());
            self.context.locals.push((name, field.clone()));
        }
        let pattern = if bindings.is_empty() {
            variant.to_owned()
        } else {
            format!("{variant}({})", bindings.join(", "))
        };
        self.line(depth, format!("{pattern} => {{"));
        let returned = self.block(rng, depth + 1);
        let comma = if chance(rng, ARM_COMMA_PERCENT) {
            ","
        } else {
            ""
        };
        self.line(depth, format!("}}{comma}"));
        self.context.locals.truncate(scope);
        returned
    }

    fn loop_statement(&mut self, rng: &mut Rng, depth: u32) {
        let decls = self.context.decls;
        let bound = decls.loop_bounds[rng.pick(decls.loop_bounds.len())];
        let name = self.name(rng, "i");
        self.line(
            depth,
            format!("loop {name} below {} {{", decls.constants[bound].0),
        );
        self.context.locals.push((name, Ty::U64));
        self.loops += 1;
        self.block(rng, depth + 1);
        self.loops -= 1;
        self.context.locals.pop();
        self.line(depth, "}".to_owned());
    }

    fn for_statement(&mut self, rng: &mut Rng, depth: u32) {
        let lists = self.context.lists();
        if lists.is_empty() {
            return;
        }
        let (list, ty) = lists[rng.pick(lists.len())].clone();
        let Ty::List(element, _) = ty else {
            unreachable!("lists() returns lists")
        };
        let name = self.name(rng, "x");
        self.line(depth, format!("for {name} in {list} {{"));
        self.context.locals.push((name, *element));
        self.loops += 1;
        self.block(rng, depth + 1);
        self.loops -= 1;
        self.context.locals.pop();
        self.line(depth, "}".to_owned());
    }
}
