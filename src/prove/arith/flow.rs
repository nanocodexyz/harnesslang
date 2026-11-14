use std::collections::BTreeSet;

use crate::ir::{Expr, LocalId, Stmt, Type};
use crate::prove::arith::{LoopKind, Prover, Quantity, State, U64_MAX, capacity, quantity_name};
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
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                self.value(&mut state, condition);
                let text = self.names.expr(condition);
                let taken = self.assume(state.clone(), condition, true);
                let skipped = self.assume(state, condition, false);
                let then_end = self.branch(format!("if {text}"), then, taken);
                let otherwise_end = self.branch(format!("else of if {text}"), otherwise, skipped);
                self.join(then_end, otherwise_end)
            }
            Stmt::Match { subject, ty, arms } => {
                self.value(&mut state, subject);
                let text = self.names.expr(subject);
                let variants = ty.variants(self.program).expect("a match subject is a sum");
                let mut end = None;
                for arm in arms {
                    let mut entered = state.clone();
                    for binding in arm.bindings.iter().flatten() {
                        self.bind(&mut entered, *binding, None);
                    }
                    let label = format!("match {text}: {}", variants[arm.variant].name);
                    let arm_end = self.branch(label, &arm.body, entered);
                    end = self.join(end, arm_end);
                }
                end
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
            Stmt::Effect { site, args, result } => {
                for arg in args {
                    self.value(&mut state, arg);
                }
                self.bind(&mut state, *result, None);
                let handler = self.handler.expect("an effect belongs to a handler");
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
            Stmt::Finish { value } => {
                self.value(&mut state, value);
                self.invariants_hold(&state, "when the handler returns", "invariant_preserved");
                None
            }
        }
    }

    fn branch(&mut self, label: String, statements: &[Stmt], state: State) -> Option<State> {
        if !linear::feasible(&state.facts, &mut self.budget).unwrap_or(true) {
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

    pub(super) fn compact(&mut self, mut state: State) -> State {
        let mut keep: BTreeSet<Var> = state.env.values().copied().collect();
        keep.extend(state.anchors.iter().copied());
        if let Ok(projected) = linear::project(&state.facts, &keep, &mut self.budget) {
            state.facts = projected;
        }
        state
    }
}
