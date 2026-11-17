use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{Expr, Stmt};
use crate::prove::arith::{
    LoopKind, Prover, Quantity, State, U64_MAX, capacity, modified, quantity_name,
};
use crate::prove::linear::{self, Extent, Linear, Var};

type Spread = (Option<i128>, Option<i128>);

struct Pass {
    anchors: BTreeMap<Quantity, Var>,
    end: Option<State>,
}

struct Summary {
    delta: BTreeMap<Quantity, Spread>,
    absolute: BTreeMap<Quantity, Spread>,
    indexed: BTreeMap<Quantity, (i128, i128, i128)>,
}

const INDEX_SLOPES: [i128; 2] = [-1, 1];

fn reads(condition: &Expr, quantities: &BTreeSet<Quantity>) -> bool {
    match condition {
        Expr::Field(field) => {
            quantities.contains(&Quantity::Field(*field))
                || quantities.contains(&Quantity::FieldLen(*field))
        }
        Expr::U64(_)
        | Expr::Bool(_)
        | Expr::Text(_)
        | Expr::Constant(_)
        | Expr::Local(_)
        | Expr::ListingNotice(_)
        | Expr::Cancelled => false,
        Expr::Member { base, .. } => reads(base, quantities),
        Expr::Construct { fields, .. }
        | Expr::TextBuiltin { args: fields, .. }
        | Expr::Call { args: fields, .. }
        | Expr::Bytes { args: fields, .. } => fields.iter().any(|field| reads(field, quantities)),
        Expr::Index { base, index } => reads(base, quantities) || reads(index, quantities),
        Expr::Copy { value, .. } => reads(value, quantities),
        Expr::Outcome {
            tag,
            value,
            reason,
            original,
            ..
        } => [tag, value, reason, original]
            .into_iter()
            .any(|part| reads(part, quantities)),
        Expr::Binary { left, right, .. }
        | Expr::With {
            list: left,
            entry: right,
        } => reads(left, quantities) || reads(right, quantities),
        Expr::Not(operand)
        | Expr::Len(operand)
        | Expr::TextLen(operand)
        | Expr::Truncate { text: operand, .. } => reads(operand, quantities),
    }
}

pub(super) fn finite(extent: Extent) -> Option<i128> {
    match extent {
        Extent::At(value) => Some(value),
        Extent::Infeasible | Extent::Unbounded => None,
    }
}

fn index_sum(slope: i128, steps: i128, per_step: i128) -> Option<i128> {
    let pairs = steps.checked_mul(steps.checked_sub(1)?)? / 2;
    pairs
        .checked_mul(slope)?
        .checked_add(steps.checked_mul(per_step)?)
}

fn iteration_closed_form(
    iteration: &mut State,
    start: Var,
    count: &Linear,
    indexed: (i128, i128, i128),
    now: Var,
) {
    let Some(count) = count.as_constant() else {
        return;
    };
    let (slope, low, high) = indexed;
    let base = Linear::var(start);
    let now = Linear::var(now);
    let last = count - 1;
    if slope < 0
        && let Some(lowest) = index_sum(slope, last, low)
        && let Some(fact) = base
            .plus(lowest.min(0))
            .and_then(|bound| bound.subtract(&now))
    {
        iteration.facts.push(fact);
    }
    if slope > 0
        && let Some(highest) = index_sum(slope, last, high)
        && let Some(fact) = base
            .plus(highest.max(0))
            .and_then(|bound| now.subtract(&bound))
    {
        iteration.facts.push(fact);
    }
}

impl Prover<'_> {
    fn quantity_max(&self, quantity: Quantity) -> i128 {
        let handler = self.handler.expect("a loop belongs to a handler");
        match quantity {
            Quantity::Field(_) | Quantity::Local(_) | Quantity::TextLen(_) => U64_MAX,
            Quantity::FieldLen(field) => {
                capacity(&self.program.state[field].ty).expect("a list has a capacity")
            }
            Quantity::LocalLen(local) => {
                capacity(&handler.locals[local].ty).expect("a list has a capacity")
            }
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

    fn iteration_start(
        &mut self,
        state: &State,
        kind: LoopKind<'_>,
        quantities: &BTreeSet<Quantity>,
        assumed: &[usize],
    ) -> (State, BTreeMap<Quantity, Var>) {
        let mut start = state.clone();
        let mut anchors = BTreeMap::new();
        for quantity in quantities {
            let name = format!(
                "{} at an iteration's start",
                quantity_name(self.program, self.handler, *quantity)
            );
            let max = self.quantity_max(*quantity);
            let var = self.fresh(&mut start, &name, max);
            start.env.insert(*quantity, var);
            start.anchors.insert(var);
            self.anchored.insert(var, *quantity);
            anchors.insert(*quantity, var);
        }
        self.bind_iteration(&mut start, kind, None);
        (self.assume_invariants(start, assumed), anchors)
    }

    fn assume_invariants(&mut self, mut state: State, assumed: &[usize]) -> State {
        let program = self.program;
        for index in assumed {
            state = self.assume(state, &program.invariants[*index].condition, true);
        }
        state
    }

    fn inductive(
        &mut self,
        state: &State,
        kind: LoopKind<'_>,
        body: &[Stmt],
        quantities: &BTreeSet<Quantity>,
    ) -> (Vec<usize>, Option<Pass>) {
        let program = self.program;
        let mut kept = Vec::new();
        let candidates: Vec<usize> = (0..program.invariants.len())
            .filter(|index| reads(&program.invariants[*index].condition, quantities))
            .collect();
        if candidates.is_empty() {
            return (kept, None);
        }
        let (free, _) = self.iteration_start(state, kind, quantities, &[]);
        for index in candidates {
            let condition = &program.invariants[index].condition;
            if !self.implied(&free, condition) && self.implied(state, condition) {
                kept.push(index);
            }
        }
        let rounds_max = kept.len();
        for _ in 0..=rounds_max {
            if kept.is_empty() {
                return (kept, None);
            }
            let (start, anchors) = self.iteration_start(state, kind, quantities, &kept);
            self.quiet += 1;
            let end = self.block(body, start);
            self.quiet -= 1;
            let Some(end) = end else {
                return (kept, Some(Pass { anchors, end: None }));
            };
            let before = kept.len();
            let mut preserved = Vec::new();
            for index in &kept {
                if self.implied(&end, &program.invariants[*index].condition) {
                    preserved.push(*index);
                }
            }
            if preserved.len() == before {
                return (
                    kept,
                    Some(Pass {
                        anchors,
                        end: Some(end),
                    }),
                );
            }
            kept = preserved;
        }
        unreachable!("every round before the last drops an invariant, so the rounds end with none")
    }

    fn summarize_body(
        &mut self,
        state: &State,
        kind: LoopKind<'_>,
        body: &[Stmt],
        quantities: &BTreeSet<Quantity>,
        assumed: &[usize],
    ) -> Option<Summary> {
        let (start, anchors) = self.iteration_start(state, kind, quantities, assumed);
        self.quiet += 1;
        let end = self.block(body, start);
        self.quiet -= 1;
        self.summary_of(kind, quantities, Pass { anchors, end })
    }

    fn summary_of(
        &mut self,
        kind: LoopKind<'_>,
        quantities: &BTreeSet<Quantity>,
        pass: Pass,
    ) -> Option<Summary> {
        let Pass { anchors, end } = pass;
        let end = end?;
        let mut summary = Summary {
            delta: BTreeMap::new(),
            absolute: BTreeMap::new(),
            indexed: BTreeMap::new(),
        };
        let index = match kind {
            LoopKind::Below { index, .. } => end.env.get(&Quantity::Local(index)).copied(),
            LoopKind::Each { .. } => None,
        };
        for quantity in quantities {
            let now = Linear::var(end.env[quantity]);
            let delta = now
                .subtract(&Linear::var(anchors[quantity]))
                .expect("a difference of two variables fits");
            let (low, high) = self.range(&end, &delta);
            let spread = (finite(low), finite(high));
            let (low, high) = self.range(&end, &now);
            let absolute = (finite(low), finite(high));
            if let Some(index) = index
                && let Some(indexed) = self.slope(&end, &delta, index, spread)
            {
                summary.indexed.insert(*quantity, indexed);
            }
            summary.delta.insert(*quantity, spread);
            summary.absolute.insert(*quantity, absolute);
        }
        Some(summary)
    }

    fn slope(
        &mut self,
        end: &State,
        delta: &Linear,
        index: Var,
        spread: Spread,
    ) -> Option<(i128, i128, i128)> {
        let mut best = match spread {
            (Some(low), Some(high)) => (0, low, high),
            _ => (0, i128::MIN, i128::MAX),
        };
        for slope in INDEX_SLOPES {
            let Some(rest) = Linear::var(index)
                .scale(slope)
                .and_then(|scaled| delta.subtract(&scaled))
            else {
                continue;
            };
            let (low, high) = self.range(end, &rest);
            if let (Some(low), Some(high)) = (finite(low), finite(high))
                && high.checked_sub(low)? < best.2.checked_sub(best.1).unwrap_or(i128::MAX)
            {
                best = (slope, low, high);
            }
        }
        (best.0 != 0).then_some(best)
    }

    fn closed_form(
        &self,
        exit: &mut State,
        start: Var,
        count: &Linear,
        indexed: (i128, i128, i128),
        now: Var,
    ) {
        let Some(count) = count.as_constant() else {
            return;
        };
        let (slope, low, high) = indexed;
        let base = Linear::var(start);
        let now = Linear::var(now);
        if let Some(lowest) = index_sum(slope, count, low)
            && let Some(fact) = base.plus(lowest).and_then(|bound| bound.subtract(&now))
        {
            exit.facts.push(fact);
        }
        if let Some(highest) = index_sum(slope, count, high)
            && let Some(fact) = base.plus(highest).and_then(|bound| now.subtract(&bound))
        {
            exit.facts.push(fact);
        }
    }

    fn advance(
        &mut self,
        state: &mut State,
        quantity: Quantity,
        start: Var,
        steps: &Linear,
        spread: Spread,
    ) {
        let name = quantity_name(self.program, self.handler, quantity);
        let max = self.quantity_max(quantity);
        let var = self.fresh(state, &name, max);
        let now = Linear::var(var);
        let base = Linear::var(start);
        let informative = |bound: &i128| bound.abs() < U64_MAX;
        let (low, high) = (spread.0.filter(informative), spread.1.filter(informative));
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

    pub(super) fn repeat(
        &mut self,
        state: State,
        kind: LoopKind<'_>,
        body: &[Stmt],
    ) -> Option<State> {
        let mut state = state;
        let count = self.iterations(&mut state, kind);
        if count.as_constant() == Some(0) {
            return Some(state);
        }
        let mut quantities = BTreeSet::new();
        modified(body, self.program, &mut quantities);
        let label = self.label(kind);
        let (assumed, pass) = self.inductive(&state, kind, body, &quantities);
        let summary = match pass {
            Some(pass) => self.summary_of(kind, &quantities, pass),
            None => self.summarize_body(&state, kind, body, &quantities, &assumed),
        };
        let mut iteration = state.clone();
        let counter = self.fresh(&mut iteration, &format!("iteration of {label}"), U64_MAX);
        let counter_value = Linear::var(counter);
        iteration.facts.push(
            counter_value
                .plus(1)
                .and_then(|next| next.subtract(&count))
                .expect("an iteration bound fits"),
        );
        if summary.is_none() {
            iteration.facts.push(counter_value.clone());
        }
        for quantity in &quantities {
            let spread = summary
                .as_ref()
                .map_or((Some(0), Some(0)), |summary| summary.delta[quantity]);
            self.advance(
                &mut iteration,
                *quantity,
                state.env[quantity],
                &counter_value,
                spread,
            );
            if let Some(indexed) = summary
                .as_ref()
                .and_then(|summary| summary.indexed.get(quantity))
            {
                let now = iteration.env[quantity];
                iteration_closed_form(&mut iteration, state.env[quantity], &count, *indexed, now);
            }
        }
        self.bind_iteration(&mut iteration, kind, Some(counter));
        let iteration = self.assume_invariants(iteration, &assumed);
        self.path.push(label);
        if self.quiet == 0 {
            self.block(body, iteration);
        }
        self.path.pop();
        let exit = self.exit(state, kind, &count, &quantities, summary.as_ref())?;
        let exit = self.assume_invariants(exit, &assumed);
        Some(self.compact(exit))
    }

    fn exit(
        &mut self,
        state: State,
        kind: LoopKind<'_>,
        count: &Linear,
        quantities: &BTreeSet<Quantity>,
        summary: Option<&Summary>,
    ) -> Option<State> {
        let mut exit = state.clone();
        let Some(summary) = summary else {
            if matches!(kind, LoopKind::Below { .. }) {
                return None;
            }
            exit.facts.push(count.clone());
            return Some(exit);
        };
        for quantity in quantities {
            self.advance(
                &mut exit,
                *quantity,
                state.env[quantity],
                count,
                summary.delta[quantity],
            );
            if let Some(indexed) = summary.indexed.get(quantity) {
                let now = exit.env[quantity];
                self.closed_form(&mut exit, state.env[quantity], count, *indexed, now);
            }
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
            left.facts.extend(
                linear::equal(&Linear::var(var), &Linear::var(left_var))
                    .expect("an equality of variables fits"),
            );
            right.facts.extend(
                linear::equal(&Linear::var(var), &Linear::var(right_var))
                    .expect("an equality of variables fits"),
            );
            env.insert(quantity, var);
            keep.insert(var);
            joined.push((var, quantity));
        }
        let anchors = left.anchors.intersection(&right.anchors).copied().collect();
        let left_facts = linear::project(&left.facts, &keep, &mut self.budget).unwrap_or_default();
        let right_facts =
            linear::project(&right.facts, &keep, &mut self.budget).unwrap_or_default();
        let left_set: BTreeSet<&Linear> = left_facts.iter().collect();
        let right_set: BTreeSet<&Linear> = right_facts.iter().collect();
        let mut facts: BTreeSet<Linear> = left_set
            .intersection(&right_set)
            .map(|fact| (*fact).clone())
            .collect();
        for fact in &left_facts {
            if !right_set.contains(fact) && self.proves_feasible(&right_facts, fact) {
                facts.insert(fact.clone());
            }
        }
        for fact in &right_facts {
            if !left_set.contains(fact) && self.proves_feasible(&left_facts, fact) {
                facts.insert(fact.clone());
            }
        }
        let sides = [
            State {
                facts: left_facts,
                env: BTreeMap::new(),
                anchors: BTreeSet::new(),
            },
            State {
                facts: right_facts,
                env: BTreeMap::new(),
                anchors: BTreeSet::new(),
            },
        ];
        let joined_set: BTreeSet<Var> = joined.iter().map(|(var, _)| *var).collect();
        let mut related: BTreeSet<(Var, Var)> = BTreeSet::new();
        for side in &sides {
            for fact in &side.facts {
                for left_var in fact.terms.keys() {
                    for right_var in fact.terms.keys() {
                        related.insert((*left_var, *right_var));
                    }
                }
            }
        }
        for (var, quantity) in &joined {
            let mut measures = vec![Linear::var(*var)];
            for other in &keep {
                let mirrored = joined_set.contains(other) && other < var;
                let foreign = self
                    .anchored
                    .get(other)
                    .is_some_and(|anchored| anchored != quantity);
                if other != var && !mirrored && !foreign && related.contains(&(*var, *other)) {
                    measures.push(
                        Linear::var(*var)
                            .subtract(&Linear::var(*other))
                            .expect("a difference of variables fits"),
                    );
                }
            }
            for measure in measures {
                facts.extend(self.hull(&sides, &measure));
            }
        }
        State {
            facts: facts.into_iter().collect(),
            env,
            anchors,
        }
    }

    fn hull(&mut self, sides: &[State; 2], measure: &Linear) -> Vec<Linear> {
        let mut low = Some(i128::MAX);
        let mut high = Some(i128::MIN);
        for side in sides {
            let (side_low, side_high) = self.range(side, measure);
            let (side_low, side_high) = (finite(side_low), finite(side_high));
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
