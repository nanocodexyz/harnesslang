use std::collections::BTreeSet;

use crate::ir::{Expr, Kept, LocalId, Stmt, Type};
use crate::prove::arith::loops::finite;
use crate::prove::arith::{
    LoopKind, Prover, Quantity, ShrinkBound, State, U64_MAX, capacity, quantity_name,
};
use crate::prove::linear::{self, Linear, Var};

impl Prover<'_> {
    pub(super) fn block(&mut self, statements: &[Stmt], state: State) -> Option<State> {
        let mut state = state;
        for statement in statements {
            state = self.statement(statement, state)?;
        }
        Some(state)
    }

    fn statement(&mut self, statement: &Stmt, mut state: State) -> Option<State> {
        match statement {
            Stmt::Let { local, value } => {
                let linear = self.local_value(&mut state, *local, value);
                self.bind(&mut state, *local, linear);
                self.bind_text(&mut state, *local, value);
                Some(self.compact(state))
            }
            Stmt::Assign { field, value } => {
                self.assign(&mut state, *field, value);
                Some(self.compact(state))
            }
            Stmt::Push { field, value } => {
                self.value(&mut state, value);
                self.push(&mut state, *field);
                Some(self.compact(state))
            }
            Stmt::Shrink { field, kept } => {
                let state = self.shrink(state, *field, kept);
                Some(self.compact(state))
            }
            Stmt::KeepBlobs { .. } => Some(state),
            Stmt::Rewrite { field, arms } => {
                let state = self.rewrite(state, *field, arms);
                Some(self.compact(state))
            }
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                self.value(&mut state, condition);
                let text = self.names.expr(condition);
                let taken = self.assume_settled(state.clone(), condition, true);
                let skipped = self.assume_settled(state.clone(), condition, false);
                let then_end = self.branch(format!("if {text}"), then, taken);
                let otherwise_end = self.branch(format!("else of if {text}"), otherwise, skipped);
                self.merge(state, vec![then_end, otherwise_end])
            }
            Stmt::Match { subject, ty, arms } => {
                self.value(&mut state, subject);
                let text = self.names.expr(subject);
                let variants = ty.variants(self.program).expect("a match subject is a sum");
                let mut ends = Vec::with_capacity(arms.len());
                let usage = self.usage_arm(ty);
                for arm in arms {
                    let mut entered = state.clone();
                    for (field, binding) in arm.bindings.iter().enumerate() {
                        let Some(binding) = binding else { continue };
                        match usage {
                            Some((variant, window))
                                if arm.variant == variant
                                    && (crate::ir::ATTEMPT_USAGE..crate::ir::ATTEMPT_OK_FIELDS)
                                        .contains(&field) =>
                            {
                                self.bind_bounded(&mut entered, *binding, window);
                            }
                            _ => self.bind(&mut entered, *binding, None),
                        }
                    }
                    let label = format!("match {text}: {}", variants[arm.variant].name);
                    ends.push(self.branch(label, &arm.body, (entered, None)));
                }
                self.merge(state, ends)
            }
            Stmt::Loop { index, bound, body } => self.repeat(
                state,
                LoopKind::Below {
                    index: *index,
                    bound,
                },
                body,
            ),
            Stmt::For {
                item,
                list,
                bound,
                body,
            } => self.repeat(
                state,
                LoopKind::Each {
                    item: *item,
                    list,
                    bound,
                },
                body,
            ),
            Stmt::Parallel(parallel) => self.statement(&parallel.sequential(), state),
            Stmt::Effect { site, args, result } => {
                for arg in args {
                    self.value(&mut state, arg);
                }
                let handler = self.handler.expect("an effect belongs to a handler");
                if let Some(cap) = handler.sites[*site].kind.sleeps() {
                    self.wait_bound(&mut state, &args[0], cap);
                }
                self.bind(&mut state, *result, None);
                if handler.sites[*site].traps_on_decode() {
                    let what = self.names.site(*site, args);
                    self.invariants_hold(
                        &state,
                        &format!("when the reply of {what} fails to decode and the turn traps"),
                        "invariant_preserved",
                    );
                }
                Some(state)
            }
            Stmt::Take { result, .. } => {
                self.bind(&mut state, *result, None);
                Some(state)
            }
            Stmt::Finish { value } => {
                self.value(&mut state, value);
                self.invariants_hold(&state, "when the handler returns", "invariant_preserved");
                None
            }
        }
    }

    fn merge(&mut self, before: State, ends: Vec<Option<State>>) -> Option<State> {
        let reached: Vec<State> = ends.into_iter().flatten().collect();
        let unchanged = |end: &State| {
            before
                .env
                .iter()
                .all(|(quantity, var)| end.env.get(quantity) == Some(var))
        };
        if reached.len() > 1 && reached.iter().all(unchanged) {
            return Some(before);
        }
        let mut end = None;
        for arm_end in reached {
            end = self.join(end, Some(arm_end));
        }
        end
    }

    fn usage_arm(&self, ty: &Type) -> Option<(usize, i128)> {
        let window = self.program.model.as_ref()?.window.as_ref()?;
        (*ty == Type::Sum(window.attempt))
            .then(|| (crate::ir::ATTEMPT_OK, i128::from(window.tokens.value)))
    }

    fn bind_text(&mut self, state: &mut State, local: LocalId, value: &Expr) {
        let handler = self.handler.expect("a local belongs to a handler");
        if handler.locals[local].ty != Type::Text {
            return;
        }
        match value {
            Expr::Truncate { tokens, .. } => {
                let max = self.cut_max(*tokens);
                self.bind_bounded(state, local, max);
            }
            Expr::Local(other) => {
                if let Some(var) = state.env.get(&Quantity::TextLen(*other)).copied() {
                    state.env.insert(Quantity::TextLen(local), var);
                }
            }
            _ => {}
        }
    }

    fn wait_bound(&mut self, state: &mut State, duration: &Expr, cap: crate::ir::ConstId) {
        let constant = &self.program.constants[cap];
        let linear = self.numeric(state, duration);
        let goal = linear
            .plus(-i128::from(constant.value))
            .expect("a wait's cap fits i128");
        let message = format!(
            "wait({}) may sleep past its cap {} and trap with bound({})",
            self.names.expr(duration),
            constant.name,
            constant.name
        );
        self.obligation(state, goal, "wait_bound", message);
    }

    fn branch(
        &mut self,
        label: String,
        statements: &[Stmt],
        settled: (State, Option<bool>),
    ) -> Option<State> {
        let (state, feasible) = settled;
        let feasible = match feasible {
            Some(known) => known,
            None => linear::feasible(&state.facts, &mut self.budget).unwrap_or(true),
        };
        if !feasible {
            return None;
        }
        self.path.push(label);
        let end = self.block(statements, state);
        self.path.pop();
        end
    }

    fn local_value(&mut self, state: &mut State, local: LocalId, value: &Expr) -> Option<Linear> {
        let handler = self.handler.expect("a local belongs to a handler");
        match handler.locals[local].ty {
            Type::List { .. } => {
                self.value(state, value);
                self.list_len(state, value)
            }
            _ => self.value(state, value),
        }
    }

    fn assign(&mut self, state: &mut State, field: usize, value: &Expr) {
        let program = self.program;
        let ty = &program.state[field].ty;
        let (quantity, max, linear) = match ty {
            Type::U64 => (Quantity::Field(field), U64_MAX, self.value(state, value)),
            Type::List { .. } => {
                self.value(state, value);
                let length = self.list_len(state, value);
                (
                    Quantity::FieldLen(field),
                    capacity(ty).expect("a list has a capacity"),
                    length,
                )
            }
            _ => {
                self.value(state, value);
                return;
            }
        };
        let name = quantity_name(program, None, quantity);
        let var = self.fresh_equal(state, &name, max, linear);
        state.env.insert(quantity, var);
    }

    fn push(&mut self, state: &mut State, field: usize) {
        let program = self.program;
        let name = program.state[field].name.clone();
        let quantity = Quantity::FieldLen(field);
        let length = Linear::var(state.env[&quantity]);
        let cap = capacity(&program.state[field].ty).expect("push targets a list");
        let goal = length.plus(1 - cap).expect("a capacity fits i128");
        let message = format!("push onto {name} may find it full and trap with bound({name})");
        self.obligation(state, goal, "push_bound", message);
        let grown = length.plus(1);
        let var = self.fresh_equal(state, &format!("len({name})"), cap, grown);
        state.env.insert(quantity, var);
    }

    fn shrink(&mut self, state: State, field: usize, kept: &Kept) -> State {
        let Kept::LastAfter { count, first } = kept else {
            return self.shrink_to(state, field, kept);
        };
        let mut state = state;
        self.value(&mut state, first);
        let mut state = self.shrink_to(state, field, &Kept::Last(count.clone()));
        self.push(&mut state, field);
        state
    }

    fn rewrite(&mut self, state: State, field: usize, arms: &[crate::ir::RewriteArm]) -> State {
        for arm in arms {
            let crate::ir::Rewritten::Into(entry) = &arm.entry else {
                continue;
            };
            let mut entered = state.clone();
            for binding in arm.bindings.iter().flatten() {
                self.bind(&mut entered, *binding, None);
            }
            self.value(&mut entered, entry);
        }
        if !crate::ir::rewrite_drops(arms) {
            return state;
        }
        let mut state = state;
        let program = self.program;
        let name = program.state[field].name.clone();
        let quantity = Quantity::FieldLen(field);
        let cap = capacity(&program.state[field].ty).expect("a rewrite targets a list");
        let length = Linear::var(state.env[&quantity]);
        let var = self.fresh(&mut state, &format!("len({name})"), cap);
        state.facts.extend(Linear::var(var).subtract(&length));
        state.env.insert(quantity, var);
        self.measure_shrink(format!("rewrite {name}"), cap, &state, None, &length, var);
        state
    }

    fn shrink_to(&mut self, mut state: State, field: usize, kept: &Kept) -> State {
        let program = self.program;
        let name = program.state[field].name.clone();
        let quantity = Quantity::FieldLen(field);
        let cap = capacity(&program.state[field].ty).expect("clear and keep target a list");
        let length = Linear::var(state.env[&quantity]);
        let label = format!("len({name})");
        let (statement, count) = match kept {
            Kept::Nothing => (format!("clear {name}"), Some(Linear::constant(0))),
            Kept::Last(count) => (
                format!("keep {name} last {}", self.names.expr(count)),
                self.value(&mut state, count),
            ),
            Kept::LastAfter { .. } => {
                unreachable!("keep after shrinks with keep last, then pushes")
            }
        };
        let Some(count) = count else {
            let var = self.fresh(&mut state, &label, cap);
            state.facts.extend(Linear::var(var).subtract(&length));
            state.env.insert(quantity, var);
            self.measure_shrink(statement, cap, &state, None, &length, var);
            return state;
        };
        let mut short = state.clone();
        short.facts.extend(length.subtract(&count));
        let mut long = state;
        long.facts
            .extend(count.plus(1).and_then(|bound| bound.subtract(&length)));
        let var = self.fresh_equal(&mut long, &label, cap, Some(count.clone()));
        long.env.insert(quantity, var);
        let dropped = length.subtract(&count);
        let joined = self
            .join(Some(short), Some(long.clone()))
            .expect("a join of two states is a state");
        let kept_var = joined.env[&quantity];
        let long = Some((&long, dropped));
        self.measure_shrink(statement, cap, &joined, long, &length, kept_var);
        joined
    }

    fn measure_shrink(
        &mut self,
        statement: String,
        capacity: i128,
        after: &State,
        long: Option<(&State, Option<Linear>)>,
        length: &Linear,
        kept: Var,
    ) {
        if self.quiet > 0 || !linear::feasible(&after.facts, &mut self.budget).unwrap_or(true) {
            return;
        }
        let left = finite(self.extent(after, &Linear::var(kept), true));
        let dropped = match long {
            Some((long, Some(dropped))) => {
                if linear::feasible(&long.facts, &mut self.budget).unwrap_or(true) {
                    finite(self.extent(long, &dropped, true))
                } else {
                    Some(0)
                }
            }
            Some((_, None)) | None => finite(self.extent(after, length, true)),
        };
        let bound = self.shrinks.entry(statement).or_insert(ShrinkBound {
            capacity,
            left,
            dropped,
        });
        bound.left = bound.left.zip(left).map(|(a, b)| a.max(b));
        bound.dropped = bound.dropped.zip(dropped).map(|(a, b)| a.max(b));
    }

    pub(super) fn compact(&mut self, mut state: State) -> State {
        let mut keep: BTreeSet<Var> = state.env.values().copied().collect();
        keep.extend(state.anchors.iter().copied());
        if let Ok(projected) = linear::project(&state.facts, &keep, &mut self.budget) {
            state.facts = projected;
        }
        state
    }
}
