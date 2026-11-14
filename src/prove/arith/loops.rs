use std::collections::{BTreeMap, BTreeSet};

use crate::ir::Stmt;
use crate::prove::arith::{LoopKind, Prover, Quantity, State, U64_MAX, capacity, modified, quantity_name};
use crate::prove::linear::{self, Extent, Linear, Var};

type Spread = (Option<i128>, Option<i128>);

struct Summary {
    delta: BTreeMap<Quantity, Spread>,
    absolute: BTreeMap<Quantity, Spread>,
}

fn finite(extent: Extent) -> Option<i128> {
    match extent {
        Extent::At(value) => Some(value),
        Extent::Infeasible | Extent::Unbounded => None,
    }
}

impl Prover<'_> {
    fn quantity_max(&self, quantity: Quantity) -> i128 {
        let handler = self.handler.expect("a loop belongs to a handler");
        match quantity {
            Quantity::Field(_) | Quantity::Local(_) => U64_MAX,
            Quantity::FieldLen(field) => capacity(&self.program.state[field].ty).expect("a list has a capacity"),
            Quantity::LocalLen(local) => capacity(&handler.locals[local].ty).expect("a list has a capacity"),
        }
    }

    fn iterations(&mut self, state: &mut State, kind: LoopKind<'_>) -> Linear {
        match kind {
            LoopKind::Below { bound, .. } => Linear::constant(i128::from(bound.value)),
            LoopKind::Each { list, bound, .. } => {
                self.value(state, list);
                if let Some(length) = self.list_len(state, list) {
                    return length;
                }
                let name = format!("len({})", self.names.expr(list));
                Linear::var(self.fresh(state, &name, i128::from(bound.value)))
            }
        }
    }

    fn label(&self, kind: LoopKind<'_>) -> String {
        let handler = self.handler.expect("a loop belongs to a handler");
        match kind {
            LoopKind::Below { index, bound } => format!(
                "loop {} below {} ({})",
                handler.locals[index].name, bound.name, bound.value
            ),
            LoopKind::Each { item, list, bound } => format!(
                "for {} in {} (at most {} = {})",
                handler.locals[item].name,
                self.names.expr(list),
                bound.name,
                bound.value
            ),
        }
    }

    fn bind_iteration(&mut self, state: &mut State, kind: LoopKind<'_>, counter: Option<Var>) {
        match kind {
            LoopKind::Below { index, bound } => {
                let var = match counter {
                    Some(var) => var,
                    None => {
                        let name = self.label(kind);
                        let var = self.fresh(state, &name, i128::from(bound.value) - 1);
                        var
                    }
                };
                state.env.insert(Quantity::Local(index), var);
            }
            LoopKind::Each { item, .. } => self.bind(state, item, None),
        }
    }

    fn summarize_body(&mut self, state: &State, kind: LoopKind<'_>, body: &[Stmt], quantities: &BTreeSet<Quantity>) -> Option<Summary> {
        let mut start = state.clone();
        let mut anchors = BTreeMap::new();
        for quantity in quantities {
            let name = format!("{} at an iteration's start", quantity_name(self.program, self.handler, *quantity));
            let max = self.quantity_max(*quantity);
            let var = self.fresh(&mut start, &name, max);
            start.env.insert(*quantity, var);
            start.anchors.insert(var);
            anchors.insert(*quantity, var);
        }
        self.bind_iteration(&mut start, kind, None);
        self.quiet += 1;
        let end = self.block(body, start);
        self.quiet -= 1;
        let end = end?;
        let mut summary = Summary {
            delta: BTreeMap::new(),
            absolute: BTreeMap::new(),
        };
        for quantity in quantities {
            let now = Linear::var(end.env[quantity]);
            let delta = now.subtract(&Linear::var(anchors[quantity])).expect("a difference of two variables fits");
            let spread = (finite(self.extent(&end, &delta, false)), finite(self.extent(&end, &delta, true)));
            let absolute = (finite(self.extent(&end, &now, false)), finite(self.extent(&end, &now, true)));
            summary.delta.insert(*quantity, spread);
            summary.absolute.insert(*quantity, absolute);
        }
        Some(summary)
    }

    fn advance(&mut self, state: &mut State, quantity: Quantity, start: Var, steps: &Linear, spread: Spread) {
        let name = quantity_name(self.program, self.handler, quantity);
        let max = self.quantity_max(quantity);
        let var = self.fresh(state, &name, max);
        let now = Linear::var(var);
        let base = Linear::var(start);
        let (low, high) = spread;
        if let Some(low) = low
            && let Some(reached) = steps.scale(low).and_then(|scaled| base.add(&scaled))
            && let Some(fact) = reached.subtract(&now)
        {
            state.facts.push(fact);
        }
        if let Some(high) = high
            && let Some(reached) = steps.scale(high).and_then(|scaled| base.add(&scaled))
            && let Some(fact) = now.subtract(&reached)
        {
            state.facts.push(fact);
        }
        state.env.insert(quantity, var);
    }

    pub(super) fn repeat(&mut self, state: State, kind: LoopKind<'_>, body: &[Stmt]) -> Option<State> {
        let mut state = state;
        let count = self.iterations(&mut state, kind);
        if count.as_constant() == Some(0) {
            return Some(state);
        }
        let mut quantities = BTreeSet::new();
        modified(body, self.program, &mut quantities);
        let summary = self.summarize_body(&state, kind, body, &quantities);
        let label = self.label(kind);
        let mut iteration = state.clone();
        let counter = self.fresh(&mut iteration, &format!("iteration of {label}"), U64_MAX);
        let counter_value = Linear::var(counter);
        iteration.facts.push(counter_value.plus(1).and_then(|next| next.subtract(&count)).expect("an iteration bound fits"));
        if summary.is_none() {
            iteration.facts.push(counter_value.clone());
        }
        for quantity in &quantities {
            let spread = summary.as_ref().map_or((Some(0), Some(0)), |summary| summary.delta[quantity]);
            self.advance(&mut iteration, *quantity, state.env[quantity], &counter_value, spread);
        }
        self.bind_iteration(&mut iteration, kind, Some(counter));
        self.path.push(label);
        self.block(body, iteration);
        self.path.pop();
        let exit = self.exit(state, kind, &count, &quantities, summary.as_ref())?;
        Some(self.compact(exit))
    }

    fn exit(&mut self, state: State, kind: LoopKind<'_>, count: &Linear, quantities: &BTreeSet<Quantity>, summary: Option<&Summary>) -> Option<State> {
        let mut exit = state.clone();
        let Some(summary) = summary else {
            if matches!(kind, LoopKind::Below { .. }) {
                return None;
            }
            exit.facts.push(count.clone());
            return Some(exit);
        };
        for quantity in quantities {
            self.advance(&mut exit, *quantity, state.env[quantity], count, summary.delta[quantity]);
            if matches!(kind, LoopKind::Below { .. }) {
                let now = Linear::var(exit.env[quantity]);
                let (low, high) = summary.absolute[quantity];
                if let Some(low) = low {
                    exit.facts.extend(Linear::constant(low).subtract(&now));
                }
                if let Some(high) = high {
                    exit.facts.extend(now.plus(-high));
                }
            }
        }
        Some(exit)
    }

    pub(super) fn join(&mut self, left: Option<State>, right: Option<State>) -> Option<State> {
        match (left, right) {
            (None, other) | (other, None) => other,
            (Some(left), Some(right)) => Some(self.join_two(left, right)),
        }
    }

    fn join_two(&mut self, mut left: State, mut right: State) -> State {
        if !linear::feasible(&left.facts, &mut self.budget).unwrap_or(true) {
            return right;
        }
        if !linear::feasible(&right.facts, &mut self.budget).unwrap_or(true) {
            return left;
        }
        let mut env = BTreeMap::new();
        let mut keep: BTreeSet<Var> = left.anchors.intersection(&right.anchors).copied().collect();
        let mut joined = Vec::new();
        for (quantity, left_var) in left.env.clone() {
            let Some(&right_var) = right.env.get(&quantity) else {
                continue;
            };
            if left_var == right_var {
                env.insert(quantity, left_var);
                keep.insert(left_var);
                continue;
            }
            let name = quantity_name(self.program, self.handler, quantity);
            let var = self.fresh_raw(&name);
            left.facts.extend(linear::equal(&Linear::var(var), &Linear::var(left_var)).expect("an equality of variables fits"));
            right.facts.extend(linear::equal(&Linear::var(var), &Linear::var(right_var)).expect("an equality of variables fits"));
            env.insert(quantity, var);
            keep.insert(var);
            joined.push(var);
        }
        let anchors = left.anchors.intersection(&right.anchors).copied().collect();
        let left_facts = linear::project(&left.facts, &keep, &mut self.budget).unwrap_or_default();
        let right_facts = linear::project(&right.facts, &keep, &mut self.budget).unwrap_or_default();
        let mut facts = BTreeSet::new();
        for fact in &left_facts {
            if self.proves(&right_facts, fact) {
                facts.insert(fact.clone());
            }
        }
        for fact in &right_facts {
            if self.proves(&left_facts, fact) {
                facts.insert(fact.clone());
            }
        }
        let sides = [
            State { facts: left_facts, env: BTreeMap::new(), anchors: BTreeSet::new() },
            State { facts: right_facts, env: BTreeMap::new(), anchors: BTreeSet::new() },
        ];
        for var in &joined {
            let mut measures = vec![Linear::var(*var)];
            for other in &keep {
                if other != var {
                    measures.push(Linear::var(*var).subtract(&Linear::var(*other)).expect("a difference of variables fits"));
                }
            }
            for measure in measures {
                facts.extend(self.hull(&sides, &measure));
            }
        }
        State { facts: facts.into_iter().collect(), env, anchors }
    }

    fn hull(&mut self, sides: &[State; 2], measure: &Linear) -> Vec<Linear> {
        let mut low = Some(i128::MAX);
        let mut high = Some(i128::MIN);
        for side in sides {
            let side_low = finite(self.extent(side, measure, false));
            let side_high = finite(self.extent(side, measure, true));
            low = low.zip(side_low).map(|(a, b)| a.min(b));
            high = high.zip(side_high).map(|(a, b)| a.max(b));
        }
        let mut facts = Vec::new();
        if let Some(low) = low
            && let Some(fact) = Linear::constant(low).subtract(measure)
        {
            facts.push(fact);
        }
        if let Some(high) = high
            && let Some(fact) = measure.plus(-high)
        {
            facts.push(fact);
        }
        facts
    }
}

