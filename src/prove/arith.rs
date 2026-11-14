mod expr;
mod flow;
mod loops;

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{BinaryOp, Bound, Expr, FieldId, Handler, LocalId, Program, Stmt, Type};
use crate::prove::linear::{self, Budget, Extent, Linear, Var};
use crate::prove::text::Names;
use crate::prove::{Finding, PROOF_CONSTRAINTS_MAX, PROOF_STEPS_PER_HANDLER_MAX, Report};

const U64_MAX: i128 = u64::MAX as i128;
const ZERO_STATE: &str = "(zero state)";
pub(super) const TEMPORARY: &str = crate::check::WILDCARD;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Quantity {
    Field(FieldId),
    FieldLen(FieldId),
    Local(LocalId),
    LocalLen(LocalId),
}

#[derive(Clone, Debug)]
struct State {
    facts: Vec<Linear>,
    env: BTreeMap<Quantity, Var>,
    anchors: BTreeSet<Var>,
}

#[derive(Clone, Copy)]
enum LoopKind<'b> {
    Below {
        index: LocalId,
        bound: &'b Bound,
    },
    Each {
        item: LocalId,
        list: &'b Expr,
        bound: &'b Bound,
    },
}

type Alternatives = Vec<Vec<Linear>>;

struct Prover<'a> {
    program: &'a Program,
    handler: Option<&'a Handler>,
    names: Names<'a>,
    vars: Vec<String>,
    budget: Budget,
    path: Vec<String>,
    quiet: u32,
    findings: Vec<Finding>,
    proved: BTreeMap<&'static str, u64>,
}

pub fn prove(program: &Program, report: &mut Report) {
    zero_state(program, report);
    for handler in &program.handlers {
        let mut prover = Prover::new(program, Some(handler));
        let mut state = prover.entry_state();
        prover.path.push("requires".to_owned());
        if let Some(requires) = &handler.requires {
            prover.value(&mut state, requires);
            state = prover.assume(state, requires, true);
        }
        prover.path.pop();
        prover.block(&handler.body, state);
        prover.summarize(report);
    }
}

fn zero_state(program: &Program, report: &mut Report) {
    if program.invariants.is_empty() {
        return;
    }
    let mut prover = Prover::new(program, None);
    let mut state = State {
        facts: Vec::new(),
        env: BTreeMap::new(),
        anchors: BTreeSet::new(),
    };
    for (field, declared) in program.state.iter().enumerate() {
        let quantity = match declared.ty {
            Type::U64 => Quantity::Field(field),
            Type::List { .. } => Quantity::FieldLen(field),
            _ => continue,
        };
        let var = prover.fresh(&mut state, &quantity_name(program, None, quantity), 0);
        state.env.insert(quantity, var);
    }
    prover.path.push("the zero state".to_owned());
    prover.invariants_hold(&state, "in the zero state", "invariant_initial");
    for finding in prover.findings {
        report.add(finding);
    }
    if let Some(count) = prover.proved.get("invariant_initial") {
        report.proved.push(format!(
            "zero state: invariant_initial: {count} invariant(s) hold"
        ));
    }
}

fn quantity_name(program: &Program, handler: Option<&Handler>, quantity: Quantity) -> String {
    let local = |local: LocalId| {
        handler.expect("a local belongs to a handler").locals[local]
            .name
            .clone()
    };
    match quantity {
        Quantity::Field(field) => program.state[field].name.clone(),
        Quantity::FieldLen(field) => format!("len({})", program.state[field].name),
        Quantity::Local(id) => local(id),
        Quantity::LocalLen(id) => format!("len({})", local(id)),
    }
}

fn capacity(ty: &Type) -> Option<i128> {
    match ty {
        Type::List { capacity, .. } => Some(i128::from(capacity.value)),
        _ => None,
    }
}

fn modified(statements: &[Stmt], program: &Program, out: &mut BTreeSet<Quantity>) {
    for statement in statements {
        match statement {
            Stmt::Assign { field, .. } => match program.state[*field].ty {
                Type::U64 => {
                    out.insert(Quantity::Field(*field));
                }
                Type::List { .. } => {
                    out.insert(Quantity::FieldLen(*field));
                }
                _ => {}
            },
            Stmt::Push { field, .. } => {
                out.insert(Quantity::FieldLen(*field));
            }
            Stmt::If {
                then, otherwise, ..
            } => {
                modified(then, program, out);
                modified(otherwise, program, out);
            }
            Stmt::Match { arms, .. } => arms
                .iter()
                .for_each(|arm| modified(&arm.body, program, out)),
            Stmt::Loop { body, .. } | Stmt::For { body, .. } => modified(body, program, out),
            Stmt::Let { .. } | Stmt::Effect { .. } | Stmt::Finish { .. } => {}
        }
    }
}

fn related(facts: &[Linear], goal: &Linear) -> BTreeSet<Var> {
    let mut reached: BTreeSet<Var> = goal.terms.keys().copied().collect();
    for _ in 0..facts.len() {
        let before = reached.len();
        for fact in facts {
            if fact.terms.keys().any(|var| reached.contains(var)) {
                reached.extend(fact.terms.keys().copied());
            }
        }
        if reached.len() == before {
            break;
        }
    }
    reached
}

fn product(left: Alternatives, right: Alternatives) -> Option<Alternatives> {
    let mut out = Vec::new();
    for one in &left {
        for other in &right {
            let mut both = one.clone();
            both.extend(other.iter().cloned());
            out.push(both);
            if out.len() > PROOF_CONSTRAINTS_MAX {
                return None;
            }
        }
    }
    Some(out)
}

impl<'a> Prover<'a> {
    fn new(program: &'a Program, handler: Option<&'a Handler>) -> Self {
        Self {
            program,
            handler,
            names: Names::new(program, handler),
            vars: Vec::new(),
            budget: Budget {
                steps_left: PROOF_STEPS_PER_HANDLER_MAX,
                constraints_max: PROOF_CONSTRAINTS_MAX,
            },
            path: Vec::new(),
            quiet: 0,
            findings: Vec::new(),
            proved: BTreeMap::new(),
        }
    }

    fn handler_name(&self) -> String {
        self.handler
            .map_or_else(|| ZERO_STATE.to_owned(), |handler| handler.name.clone())
    }

    fn summarize(self, report: &mut Report) {
        let name = self.handler_name();
        let errors = !self.findings.is_empty();
        let count = |property| self.proved.get(property).copied().unwrap_or(0);
        if !errors {
            report.proved.push(format!(
                "handler {name}: no_overflow: {} arithmetic operation(s) proved in range; invariant_preserved: {} invariant check(s) at returns and trap points; push_bound: {} push(es) proved below capacity ({} prover steps)",
                count("no_overflow"),
                count("invariant_preserved"),
                count("push_bound"),
                PROOF_STEPS_PER_HANDLER_MAX - self.budget.steps_left
            ));
        }
        for finding in self.findings {
            report.add(finding);
        }
    }

    fn fresh_raw(&mut self, name: &str) -> Var {
        self.vars.push(name.to_owned());
        self.vars.len() - 1
    }

    fn fresh(&mut self, state: &mut State, name: &str, max: i128) -> Var {
        let var = self.fresh_raw(name);
        let value = Linear::var(var);
        state
            .facts
            .push(value.scale(-1).expect("negating one variable fits"));
        state
            .facts
            .push(value.plus(-max).expect("a u64 bound fits i128"));
        var
    }

    fn fresh_equal(
        &mut self,
        state: &mut State,
        name: &str,
        max: i128,
        value: Option<Linear>,
    ) -> Var {
        let var = self.fresh(state, name, max);
        if let Some(value) = value
            && let Some(pair) = linear::equal(&Linear::var(var), &value)
        {
            state.facts.extend(pair);
        }
        var
    }

    fn entry_state(&mut self) -> State {
        let mut state = State {
            facts: Vec::new(),
            env: BTreeMap::new(),
            anchors: BTreeSet::new(),
        };
        let program = self.program;
        for (field, declared) in program.state.iter().enumerate() {
            let (quantity, max) = match &declared.ty {
                Type::U64 => (Quantity::Field(field), U64_MAX),
                ty @ Type::List { .. } => (
                    Quantity::FieldLen(field),
                    capacity(ty).expect("a list has a capacity"),
                ),
                _ => continue,
            };
            let var = self.fresh(&mut state, &quantity_name(program, None, quantity), max);
            state.env.insert(quantity, var);
        }
        let handler = self.handler.expect("an entry state belongs to a handler");
        for param in &handler.params {
            self.bind(&mut state, *param, None);
        }
        for invariant in &program.invariants {
            self.assume_in_range(&mut state, &invariant.condition);
            state = self.assume(state, &invariant.condition, true);
        }
        state
    }

    fn bind(&mut self, state: &mut State, local: LocalId, value: Option<Linear>) {
        let handler = self.handler.expect("a local belongs to a handler");
        let ty = &handler.locals[local].ty;
        let name = if handler.locals[local].name.starts_with(TEMPORARY) {
            format!("{TEMPORARY}{}", self.names.local(local))
        } else {
            handler.locals[local].name.clone()
        };
        match ty {
            Type::U64 => {
                let var = self.fresh_equal(state, &name, U64_MAX, value);
                state.env.insert(Quantity::Local(local), var);
            }
            Type::List { .. } => {
                let max = capacity(ty).expect("a list has a capacity");
                let var = self.fresh_equal(state, &format!("len({name})"), max, value);
                state.env.insert(Quantity::LocalLen(local), var);
            }
            _ => {}
        }
    }

    fn list_len(&self, state: &State, list: &Expr) -> Option<Linear> {
        let quantity = match list {
            Expr::Field(field) => Quantity::FieldLen(*field),
            Expr::Local(local) => Quantity::LocalLen(*local),
            _ => return None,
        };
        state.env.get(&quantity).map(|var| Linear::var(*var))
    }

    fn linear_of(&self, state: &State, value: &Expr) -> Option<Linear> {
        match value {
            Expr::U64(number) => Some(Linear::constant(i128::from(*number))),
            Expr::Constant(constant) => Some(Linear::constant(i128::from(
                self.program.constants[*constant].value,
            ))),
            Expr::Local(local) => state
                .env
                .get(&Quantity::Local(*local))
                .map(|var| Linear::var(*var)),
            Expr::Field(field) => state
                .env
                .get(&Quantity::Field(*field))
                .map(|var| Linear::var(*var)),
            Expr::Len(list) => self.list_len(state, list),
            Expr::Binary {
                op, left, right, ..
            } => {
                let left = self.linear_of(state, left)?;
                let right = self.linear_of(state, right)?;
                match op {
                    BinaryOp::Add => left.add(&right),
                    BinaryOp::Subtract => left.subtract(&right),
                    BinaryOp::Multiply => match (left.as_constant(), right.as_constant()) {
                        (Some(factor), _) => right.scale(factor),
                        (_, Some(factor)) => left.scale(factor),
                        _ => None,
                    },
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn report(&mut self, property: &'static str, message: String, counterexample: Option<String>) {
        if self.quiet > 0 {
            return;
        }
        let path = if self.path.is_empty() {
            "entry".to_owned()
        } else {
            self.path.join(" > ")
        };
        let finding = Finding {
            handler: self.handler_name(),
            target: None,
            property,
            message,
            path,
            counterexample,
        };
        if !self.findings.contains(&finding) {
            self.findings.push(finding);
        }
    }

    fn proves(&mut self, facts: &[Linear], goal: &Linear) -> bool {
        linear::implies(facts, goal, &mut self.budget).unwrap_or(false)
    }

    fn counterexample(&mut self, state: &State, goal: &Linear) -> Option<String> {
        let mut system = state.facts.clone();
        system.push(linear::negation(goal)?);
        let values = linear::model(&system, &mut self.budget).ok()??;
        let related = related(&state.facts, goal);
        let mut shown = Vec::new();
        let mut seen = BTreeSet::new();
        for var in state.env.values() {
            if let Some(value) = values.get(var)
                && related.contains(var)
                && !self.vars[*var].starts_with(TEMPORARY)
                && seen.insert(*var)
            {
                shown.push(format!("{} = {value}", self.vars[*var]));
            }
        }
        if shown.is_empty() {
            return None;
        }
        Some(shown.join(", "))
    }

    fn obligation(
        &mut self,
        state: &mut State,
        goal: Linear,
        property: &'static str,
        message: String,
    ) -> bool {
        let proven = self.proves(&state.facts, &goal);
        if proven {
            if self.quiet == 0 {
                *self.proved.entry(property).or_insert(0) += 1;
            }
        } else if self.quiet == 0 {
            let counterexample = self.counterexample(state, &goal);
            let hint = if self.budget.steps_left == 0 {
                format!(
                    "{message} (the proof budget of {PROOF_STEPS_PER_HANDLER_MAX} steps ran out)"
                )
            } else {
                message
            };
            self.report(property, hint, counterexample);
        }
        state.facts.push(goal);
        proven
    }

    fn extent(&mut self, state: &State, value: &Linear, upper: bool) -> Extent {
        let fresh = self.fresh_raw("_measure");
        let measured = if upper {
            linear::upper(&state.facts, value, fresh, &mut self.budget)
        } else {
            linear::lower(&state.facts, value, fresh, &mut self.budget)
        };
        measured.unwrap_or(Extent::Unbounded)
    }
}
