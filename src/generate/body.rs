use crate::generate::decls::{field_type, shuffle};
use crate::generate::expr::Context;
use crate::generate::names::fresh;
use crate::generate::{CALL, Decls, ENTRY, REPLY, Ty, chance, weighted};
use crate::sim::rng::Rng;

const GENERATED_STATEMENTS_MAX: u64 = 40;
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
    Clear,
    Keep,
    Rewrite,
}

const STATEMENT_WEIGHTS: [(Kind, u64); 11] = [
    (Kind::Let, 5),
    (Kind::Assign, 4),
    (Kind::Push, 4),
    (Kind::If, 3),
    (Kind::Match, 5),
    (Kind::Loop, 2),
    (Kind::For, 2),
    (Kind::Return, 1),
    (Kind::Clear, 1),
    (Kind::Keep, 1),
    (Kind::Rewrite, 1),
];
const REWRITE_ARM_PERCENT: u64 = 50;
const REWRITE_DROP_PERCENT: u64 = 30;
const HOOK_PERCENT: u64 = 25;
const HOOK_STATEMENTS_MAX: u64 = 3;
const LIFECYCLE_HOOKS: [&str; 2] = ["on session start", "on stop"];
const PARALLEL_PERCENT: u64 = 50;
const WALK_CALLS_PERCENT: u64 = 60;
const ALLOW_PERCENT: u64 = 30;
const DENY_PERCENT: u64 = 70;
const RESPOND_PERCENT: u64 = 40;

struct Body<'a> {
    context: Context<'a>,
    used: std::collections::BTreeSet<String>,
    names: u64,
    pushes: std::collections::BTreeMap<String, u64>,
    multiplier: u64,
    statements_left: u64,
    loops: u32,
    returns: Ty,
    lines: Vec<String>,
}

pub fn invariants(rng: &mut Rng, decls: &Decls) -> Vec<String> {
    let mut context = Context {
        decls,
        locals: Vec::new(),
        indices: Vec::new(),
        effects: false,
        fields: true,
        subject: false,
        lane: false,
    };
    let lists: Vec<(String, Ty)> = context.lists();
    (0..rng.below(INVARIANTS_MAX + 1))
        .map(|index| {
            let condition = if decls.provable {
                provable_invariant(rng, decls, &lists)
            } else {
                context.boolean(rng, EXPRESSION_DEPTH_MAX)
            };
            format!("invariant inv{index}: {condition}")
        })
        .collect()
}

pub fn hooks(rng: &mut Rng, decls: &Decls) -> Vec<String> {
    let mut lines = Vec::new();
    for hook in LIFECYCLE_HOOKS {
        if !chance(rng, HOOK_PERCENT) {
            continue;
        }
        let mut context = Context {
            decls,
            locals: Vec::new(),
            indices: Vec::new(),
            effects: true,
            fields: true,
            subject: false,
            lane: false,
        };
        let fields: Vec<(String, Ty)> = decls
            .fields
            .iter()
            .filter(|(_, ty)| matches!(ty, Ty::U64 | Ty::Bool | Ty::Text))
            .cloned()
            .collect();
        if fields.is_empty() {
            continue;
        }
        lines.push(String::new());
        lines.push(format!("{hook} {{"));
        for _ in 0..rng.range(1, HOOK_STATEMENTS_MAX) {
            let (name, ty) = fields[rng.pick(fields.len())].clone();
            if let Some(value) = context.expr(rng, &ty, true, EXPRESSION_DEPTH_MAX) {
                lines.push(format!("{INDENT}{name} = {value}"));
            }
        }
        lines.push("}".to_owned());
    }
    lines
}

pub fn dispatch_hooks(rng: &mut Rng, decls: &Decls) -> Vec<String> {
    if !decls.dispatch_hooks {
        return Vec::new();
    }
    let mut used = decls.names.clone();
    let call = fresh(rng, &mut used, "c".to_owned());
    let outcome = fresh(rng, &mut used, "outcome".to_owned());
    let before = decls.answering || rng.below(2) == 0;
    let after = !decls.answering && (!before || rng.below(2) == 0);
    let mut lines = Vec::new();
    let mut context = Context {
        decls,
        locals: vec![(call.clone(), Ty::Sum(CALL))],
        indices: Vec::new(),
        effects: false,
        fields: true,
        subject: false,
        lane: false,
    };
    if before {
        lines.push(String::new());
        lines.push(format!("before tool({call}: call) {{"));
        lines.extend(decision(rng, &mut context, 1));
        if chance(rng, RESPOND_PERCENT) {
            let condition = context.boolean(rng, EXPRESSION_DEPTH_MAX);
            let answer = context.text_expr(rng);
            lines.push(format!("{INDENT}if {condition} {{"));
            lines.push(format!("{INDENT}{INDENT}respond({answer})"));
            lines.push(format!("{INDENT}}}"));
        }
        if !decls.answering && chance(rng, ALLOW_PERCENT) {
            let input = context.text_expr(rng);
            lines.push(format!("{INDENT}allow({input})"));
        }
        lines.push("}".to_owned());
    }
    if after {
        lines.push(String::new());
        lines.push(format!(
            "after tool({call}: call, {outcome}: maybe<text>) {{"
        ));
        let output = fresh(rng, &mut used, "o".to_owned());
        lines.push(format!("{INDENT}match {outcome} {{"));
        lines.push(format!("{INDENT}{INDENT}ok({output}) => {{"));
        context.locals.push((output, Ty::Text));
        lines.extend(decision(rng, &mut context, 3));
        context.locals.pop();
        lines.push(format!("{INDENT}{INDENT}}}"));
        lines.push(format!("{INDENT}{INDENT}failed(_) => {{"));
        lines.extend(decision(rng, &mut context, 3));
        lines.push(format!("{INDENT}{INDENT}}}"));
        lines.push(format!("{INDENT}{INDENT}unknown => {{}}"));
        lines.push(format!("{INDENT}}}"));
        lines.push("}".to_owned());
    }
    lines
}

fn decision(rng: &mut Rng, context: &mut Context, depth: usize) -> Vec<String> {
    let indent = INDENT.repeat(depth);
    if !chance(rng, DENY_PERCENT) {
        return Vec::new();
    }
    let condition = context.boolean(rng, EXPRESSION_DEPTH_MAX);
    let reason = context.text_expr(rng);
    vec![
        format!("{indent}if {condition} {{"),
        format!("{indent}{INDENT}deny({reason})"),
        format!("{indent}}}"),
    ]
}

fn provable_invariant(rng: &mut Rng, decls: &Decls, lists: &[(String, Ty)]) -> String {
    if lists.is_empty() {
        return "true".to_owned();
    }
    let (name, ty) = &lists[rng.pick(lists.len())];
    let Ty::List(_, capacity) = ty else {
        unreachable!("lists() returns lists")
    };
    format!("len({name}) <= {}", decls.constants[*capacity].0)
}

fn param_type(rng: &mut Rng, decls: &Decls) -> Ty {
    field_type(rng, decls, decls.sums.len())
}

fn return_type(rng: &mut Rng, decls: &Decls) -> Ty {
    let context = Context {
        decls,
        locals: Vec::new(),
        indices: Vec::new(),
        effects: false,
        fields: true,
        subject: false,
        lane: false,
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
            indices: Vec::new(),
            effects: false,
            fields: true,
            subject: false,
            lane: false,
        },
        used,
        names: 0,
        pushes: std::collections::BTreeMap::new(),
        multiplier: 1,
        statements_left: GENERATED_STATEMENTS_MAX,
        loops: 0,
        returns: returns.clone(),
        lines: Vec::new(),
    };
    body.lines.push(format!(
        "on {name}({}) -> {}",
        rendered.join(", "),
        decls.render_type(&returns)
    ));
    let requires = (chance(rng, REQUIRES_PERCENT) && !decls.provable)
        .then(|| body.context.boolean(rng, EXPRESSION_DEPTH_MAX));
    body.context.effects = true;
    body.lines.push("{".to_owned());
    let mut returned = false;
    if decls.dispatch_hooks && index == 0 {
        body.statements_left -= 1;
        returned = body.dispatch_match(rng, 1, "call(\"t0\", \"{}\", \"c0\")");
    }
    if !returned {
        returned = body.statements(rng, 1);
    }
    if !returned {
        body.final_return(rng);
    }
    body.lines.push("}".to_owned());
    let requires = requires.or_else(|| body.room());
    if let Some(condition) = requires {
        body.lines
            .insert(1, format!("{INDENT}requires {condition}"));
    }
    body.lines
}

impl Body<'_> {
    fn room(&self) -> Option<String> {
        let decls = self.context.decls;
        let conditions: Vec<String> = self
            .pushes
            .iter()
            .map(|(name, pushes)| {
                let (_, ty) = decls
                    .fields
                    .iter()
                    .find(|(field, _)| field == name)
                    .expect("pushes go onto fields");
                let Ty::List(_, capacity) = ty else {
                    unreachable!("pushes go onto lists")
                };
                format!("len({name}) + {pushes} <= {}", decls.constants[*capacity].0)
            })
            .collect();
        (!conditions.is_empty()).then(|| conditions.join("\n    and "))
    }

    fn repeated(&mut self, rng: &mut Rng, depth: u32, times: u64) {
        let saved = self.multiplier;
        self.multiplier = self.multiplier.saturating_mul(times);
        self.loops += 1;
        self.block(rng, depth + 1);
        self.loops -= 1;
        self.multiplier = saved;
    }

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
                Kind::Return => depth > 1 && !self.context.lane,
                _ => true,
            })
            .collect();
        let kind = weighted(rng, &picks);
        match kind {
            Kind::Let => self.let_statement(rng, depth),
            Kind::Assign => self.assign(rng, depth),
            Kind::Push => self.push(rng, depth),
            Kind::If => return self.if_statement(rng, depth, "if"),
            Kind::Match => return self.match_statement(rng, depth),
            Kind::Loop => self.loop_statement(rng, depth),
            Kind::For => self.for_statement(rng, depth),
            Kind::Clear | Kind::Keep => self.shrink(rng, depth, kind),
            Kind::Rewrite => self.rewrite(rng, depth),
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
            let count = self.pushes.entry(name).or_insert(0);
            *count = count.saturating_add(self.multiplier);
        }
    }

    fn rewrite(&mut self, rng: &mut Rng, depth: u32) {
        let decls = self.context.decls;
        let lists: Vec<(String, Ty)> = decls
            .fields
            .iter()
            .filter_map(|(name, ty)| match ty {
                Ty::List(element, _) if matches!(**element, Ty::Sum(_)) => {
                    Some((name.clone(), (**element).clone()))
                }
                _ => None,
            })
            .collect();
        if lists.is_empty() {
            return;
        }
        let (name, element) = lists[rng.pick(lists.len())].clone();
        let variants = decls.variants(&element).expect("a sum has variants");
        let effects = std::mem::replace(&mut self.context.effects, false);
        self.line(depth, format!("rewrite {name} {{"));
        for (variant, fields) in variants {
            if !chance(rng, REWRITE_ARM_PERCENT) {
                continue;
            }
            let scope = self.context.locals.len();
            let mut bindings = Vec::new();
            for field in &fields {
                let binding = self.name(rng, "r");
                bindings.push(binding.clone());
                self.context.locals.push((binding, field.clone()));
            }
            let pattern = if bindings.is_empty() {
                variant.clone()
            } else {
                format!("{variant}({})", bindings.join(", "))
            };
            let target = if chance(rng, REWRITE_DROP_PERCENT) {
                Some("drop".to_owned())
            } else {
                self.context.expr(rng, &element, true, EXPRESSION_DEPTH_MAX)
            };
            self.context.locals.truncate(scope);
            if let Some(target) = target {
                self.line(depth + 1, format!("{pattern} => {target}"));
            }
        }
        self.line(depth, "}".to_owned());
        self.context.effects = effects;
    }

    fn shrink(&mut self, rng: &mut Rng, depth: u32, kind: Kind) {
        let lists: Vec<String> = self
            .context
            .decls
            .fields
            .iter()
            .filter(|(_, ty)| matches!(ty, Ty::List(..)))
            .map(|(name, _)| name.clone())
            .collect();
        if lists.is_empty() {
            return;
        }
        let name = lists[rng.pick(lists.len())].clone();
        if kind == Kind::Clear {
            self.line(depth, format!("clear {name}"));
            return;
        }
        let count = self.context.number(rng, EXPRESSION_DEPTH_MAX);
        self.line(depth, format!("keep {name} last {count}"));
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
        let returned = self.walk_calls(rng, depth + 1, scope) || self.block(rng, depth + 1);
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

    fn dispatch_match(&mut self, rng: &mut Rng, depth: u32, call: &str) -> bool {
        self.line(depth, format!("match dispatch({call}) {{"));
        let mut variants = self
            .context
            .decls
            .variants(&Ty::Maybe(Box::new(Ty::Text)))
            .expect("an outcome has variants");
        shuffle(rng, &mut variants);
        let mut all_return = true;
        for (variant, fields) in variants {
            all_return &= self.arm(rng, depth + 1, &variant, &fields);
        }
        self.line(depth, "}".to_owned());
        all_return
    }

    fn walk_calls(&mut self, rng: &mut Rng, depth: u32, scope: usize) -> bool {
        let calls = self.context.locals[scope..]
            .iter()
            .find_map(|(name, ty)| match ty {
                Ty::List(element, capacity) if **element == Ty::Sum(CALL) => {
                    Some((name.clone(), *capacity))
                }
                _ => None,
            });
        let Some((list, capacity)) = calls else {
            return false;
        };
        if self.context.lane || self.loops >= LOOP_NESTING_MAX || !chance(rng, WALK_CALLS_PERCENT) {
            return false;
        }
        self.statements_left = self.statements_left.saturating_sub(1);
        let times = self.context.decls.constants[capacity].1;
        if self.lanes_allowed() && chance(rng, PARALLEL_PERCENT) {
            self.parallel_statement(rng, depth, &list, times);
            return false;
        }
        let name = self.name(rng, "x");
        self.line(depth, format!("for {name} in {list} {{"));
        let saved = self.multiplier;
        self.multiplier = self.multiplier.saturating_mul(times);
        self.loops += 1;
        self.context.locals.push((name.clone(), Ty::Sum(CALL)));
        self.dispatch_match(rng, depth + 1, &name);
        self.context.locals.pop();
        self.loops -= 1;
        self.multiplier = saved;
        self.line(depth, "}".to_owned());
        false
    }

    fn lanes_allowed(&self) -> bool {
        !self.context.decls.dispatch_hooks || self.context.decls.answering
    }

    fn parallel_statement(&mut self, rng: &mut Rng, depth: u32, list: &str, times: u64) {
        let name = self.name(rng, "x");
        self.line(depth, format!("for {name} in {list} parallel {{"));
        let saved = self.multiplier;
        self.multiplier = self.multiplier.saturating_mul(times);
        self.loops += 1;
        self.context.lane = true;
        self.context.locals.push((name.clone(), Ty::Sum(CALL)));
        self.dispatch_match(rng, depth + 1, &name);
        self.context.locals.pop();
        self.context.lane = false;
        self.loops -= 1;
        self.multiplier = saved;
        self.line(depth, "}".to_owned());
    }

    fn for_statement(&mut self, rng: &mut Rng, depth: u32) {
        let lists = self.context.lists();
        if lists.is_empty() {
            return;
        }
        let (list, ty) = lists[rng.pick(lists.len())].clone();
        let Ty::List(element, capacity) = ty else {
            unreachable!("lists() returns lists")
        };
        let times = self.context.decls.constants[capacity].1;
        let parallel = *element == Ty::Sum(CALL)
            && !self.context.lane
            && self.lanes_allowed()
            && chance(rng, PARALLEL_PERCENT);
        if parallel {
            self.parallel_statement(rng, depth, &list, times);
            return;
        }
        let name = self.name(rng, "x");
        self.line(depth, format!("for {name} in {list} {{"));
        self.context.locals.push((name, *element));
        self.repeated(rng, depth, times);
        self.context.locals.pop();
        self.line(depth, "}".to_owned());
    }
}
