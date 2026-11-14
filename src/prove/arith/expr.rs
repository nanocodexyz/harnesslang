use crate::ir::{BinaryOp, Expr, Type};
use crate::prove::arith::{Alternatives, Prover, State, TEMPORARY, U64_MAX, product};
use crate::prove::linear::{self, Extent, Linear};

impl Prover<'_> {
    pub(super) fn numeric(&mut self, state: &mut State, value: &Expr) -> Linear {
        if let Some(linear) = self.value(state, value) {
            return linear;
        }
        let name = self.names.expr(value);
        Linear::var(self.fresh(state, &name, U64_MAX))
    }

    pub(super) fn value(&mut self, state: &mut State, value: &Expr) -> Option<Linear> {
        match value {
            Expr::Binary {
                op: op @ (BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply),
                left,
                right,
                ..
            } => {
                let left_value = self.numeric(state, left);
                let right_value = self.numeric(state, right);
                let text = self.names.expr(value);
                match op {
                    BinaryOp::Add => self.add(state, &left_value, &right_value, text),
                    BinaryOp::Subtract => self.subtract(state, &left_value, &right_value, text),
                    _ => Some(self.multiply(state, &left_value, &right_value, text)),
                }
            }
            Expr::Binary { left, right, .. } => {
                self.value(state, left);
                self.value(state, right);
                None
            }
            Expr::Not(operand) | Expr::Member { base: operand, .. } => {
                self.value(state, operand);
                None
            }
            Expr::Construct { fields, .. } | Expr::TextBuiltin { args: fields, .. } => {
                for field in fields {
                    self.value(state, field);
                }
                None
            }
            Expr::Len(list) => {
                self.value(state, list);
                self.linear_of(state, value)
            }
            Expr::TextLen(text) => {
                self.value(state, text);
                if let Expr::Truncate { tokens, .. } = &**text {
                    let max = self.cut_max(*tokens);
                    let name = self.names.expr(value);
                    return Some(Linear::var(self.fresh(state, &name, max)));
                }
                self.linear_of(state, value)
            }
            Expr::Truncate { text: operand, .. } => {
                self.value(state, operand);
                None
            }
            Expr::With { list, entry } => {
                self.value(state, list);
                self.value(state, entry);
                None
            }
            Expr::U64(_) | Expr::Constant(_) | Expr::Local(_) | Expr::Field(_) => {
                self.linear_of(state, value)
            }
            Expr::Bool(_) | Expr::Text(_) | Expr::Cancelled | Expr::ListingNotice(_) => None,
        }
    }

    fn opaque(&mut self, state: &mut State, value: &Linear) -> Linear {
        let low = self.extent(state, value, false);
        let high = self.extent(state, value, true);
        let var = Linear::var(self.fresh(state, &format!("{TEMPORARY}operand"), U64_MAX));
        if let Extent::At(low) = low {
            state.facts.extend(Linear::constant(low).subtract(&var));
        }
        if let Extent::At(high) = high {
            state.facts.extend(var.plus(-high));
        }
        var
    }

    fn formed<T>(
        &mut self,
        state: &mut State,
        left: &Linear,
        right: &Linear,
        form: impl Fn(&Linear, &Linear) -> Option<T>,
    ) -> T {
        if let Some(formed) = form(left, right) {
            return formed;
        }
        let left = self.opaque(state, left);
        let right = self.opaque(state, right);
        form(&left, &right).expect("two u64 variables combine within i128")
    }

    fn add(
        &mut self,
        state: &mut State,
        left: &Linear,
        right: &Linear,
        text: String,
    ) -> Option<Linear> {
        let (sum, goal) = self.formed(state, left, right, |left, right| {
            let sum = left.add(right)?;
            let goal = sum.plus(-U64_MAX)?;
            Some((sum, goal))
        });
        self.obligation(
            state,
            goal,
            "no_overflow",
            format!("u64 overflow in '{text}' is possible"),
        );
        Some(sum)
    }

    fn subtract(
        &mut self,
        state: &mut State,
        left: &Linear,
        right: &Linear,
        text: String,
    ) -> Option<Linear> {
        let (difference, goal) = self.formed(state, left, right, |left, right| {
            Some((left.subtract(right)?, right.subtract(left)?))
        });
        self.obligation(
            state,
            goal,
            "no_overflow",
            format!("u64 underflow in '{text}' is possible"),
        );
        Some(difference)
    }

    fn multiply(
        &mut self,
        state: &mut State,
        left: &Linear,
        right: &Linear,
        text: String,
    ) -> Linear {
        let message = format!("u64 overflow in '{text}' is possible");
        let scaled = match (left.as_constant(), right.as_constant()) {
            (Some(factor), _) => right.scale(factor),
            (_, Some(factor)) => left.scale(factor),
            _ => None,
        };
        if let Some(scaled) = scaled
            && let Some(goal) = scaled.plus(-U64_MAX)
        {
            self.obligation(state, goal, "no_overflow", message);
            return scaled;
        }
        let high_left = self.extent(state, left, true);
        let high_right = self.extent(state, right, true);
        let high = match (high_left, high_right) {
            (Extent::At(a), Extent::At(b)) => a.checked_mul(b).filter(|value| *value <= U64_MAX),
            (Extent::Infeasible, _) | (_, Extent::Infeasible) => Some(0),
            _ => None,
        };
        let product_name = text.clone();
        match high {
            Some(high) => {
                if self.quiet == 0 {
                    *self.proved.entry("no_overflow").or_insert(0) += 1;
                }
                Linear::var(self.fresh(state, &product_name, high))
            }
            None => {
                let describe = |extent: Extent| match extent {
                    Extent::At(value) => value.to_string(),
                    _ => "unbounded".to_owned(),
                };
                let counterexample = format!(
                    "the factors reach {} and {}",
                    describe(high_left),
                    describe(high_right)
                );
                self.report("no_overflow", message, Some(counterexample));
                Linear::var(self.fresh(state, &product_name, U64_MAX))
            }
        }
    }

    fn comparison(
        &self,
        state: &State,
        op: BinaryOp,
        left: &Expr,
        right: &Expr,
        truth: bool,
    ) -> Option<Alternatives> {
        let left = self.linear_of(state, left)?;
        let right = self.linear_of(state, right)?;
        let below = |low: &Linear, high: &Linear| low.subtract(high)?.plus(1);
        let at_most = |low: &Linear, high: &Linear| low.subtract(high);
        let alternatives = match (op, truth) {
            (BinaryOp::Less, true) | (BinaryOp::GreaterEqual, false) => {
                vec![vec![below(&left, &right)?]]
            }
            (BinaryOp::Less, false) | (BinaryOp::GreaterEqual, true) => {
                vec![vec![at_most(&right, &left)?]]
            }
            (BinaryOp::LessEqual, true) | (BinaryOp::Greater, false) => {
                vec![vec![at_most(&left, &right)?]]
            }
            (BinaryOp::LessEqual, false) | (BinaryOp::Greater, true) => {
                vec![vec![below(&right, &left)?]]
            }
            (BinaryOp::Equal, true) | (BinaryOp::NotEqual, false) => {
                vec![linear::equal(&left, &right)?.to_vec()]
            }
            (BinaryOp::Equal, false) | (BinaryOp::NotEqual, true) => {
                vec![vec![below(&left, &right)?], vec![below(&right, &left)?]]
            }
            _ => return None,
        };
        Some(alternatives)
    }

    pub(super) fn goals(
        &self,
        state: &State,
        condition: &Expr,
        truth: bool,
    ) -> Option<Alternatives> {
        match condition {
            Expr::Bool(value) => Some(if *value == truth {
                vec![Vec::new()]
            } else {
                Vec::new()
            }),
            Expr::Not(operand) => self.goals(state, operand, !truth),
            Expr::Binary {
                op: op @ (BinaryOp::And | BinaryOp::Or),
                left,
                right,
                ..
            } => {
                let left = self.goals(state, left, truth)?;
                let right = self.goals(state, right, truth)?;
                if (*op == BinaryOp::And) == truth {
                    product(left, right)
                } else {
                    let mut both = left;
                    both.extend(right);
                    Some(both)
                }
            }
            Expr::Binary {
                op,
                operand: Type::U64,
                left,
                right,
            } => self.comparison(state, *op, left, right, truth),
            _ => None,
        }
    }

    pub(super) fn assume(&mut self, mut state: State, condition: &Expr, truth: bool) -> State {
        let Some(alternatives) = self.goals(&state, condition, truth) else {
            return state;
        };
        let mut possible = Vec::new();
        for alternative in alternatives {
            let mut system = state.facts.clone();
            system.extend(alternative.iter().cloned());
            if linear::feasible(&system, &mut self.budget).unwrap_or(true) {
                possible.push(alternative);
            }
        }
        match possible.len() {
            0 => state.facts.push(Linear::constant(1)),
            1 => state.facts.extend(possible.remove(0)),
            _ => {}
        }
        state
    }

    pub(super) fn implied(&mut self, state: &State, condition: &Expr) -> bool {
        let Some(alternatives) = self.goals(state, condition, true) else {
            return false;
        };
        alternatives.iter().any(|alternative| {
            alternative
                .iter()
                .all(|goal| self.proves(&state.facts, goal))
        })
    }

    pub(super) fn holds(&mut self, state: &State, condition: &Expr) -> Result<(), Option<String>> {
        let Some(alternatives) = self.goals(state, condition, true) else {
            return Err(Some(
                "the prover reasons only about u64 comparisons joined by and, or and not"
                    .to_owned(),
            ));
        };
        let mut first_failure = None;
        for alternative in &alternatives {
            let mut failed = None;
            for goal in alternative {
                if !self.proves(&state.facts, goal) {
                    failed = Some(goal.clone());
                    break;
                }
            }
            match failed {
                None => return Ok(()),
                Some(goal) => {
                    first_failure.get_or_insert(goal);
                }
            }
        }
        Err(first_failure.and_then(|goal| self.counterexample(state, &goal)))
    }

    fn range_goals(&self, state: &State, expr: &Expr, out: &mut Vec<(Option<Linear>, String)>) {
        let (op, left, right) = match expr {
            Expr::Binary {
                op, left, right, ..
            } => (*op, left, right),
            Expr::Not(operand) => return self.range_goals(state, operand, out),
            _ => return,
        };
        self.range_goals(state, left, out);
        self.range_goals(state, right, out);
        let text = self.names.expr(expr);
        let operands = self
            .linear_of(state, left)
            .zip(self.linear_of(state, right));
        let goal = match op {
            BinaryOp::Add => operands.and_then(|(left, right)| left.add(&right)?.plus(-U64_MAX)),
            BinaryOp::Subtract => operands.and_then(|(left, right)| right.subtract(&left)),
            BinaryOp::Multiply => operands.and_then(|(left, right)| {
                let scaled = match (left.as_constant(), right.as_constant()) {
                    (Some(factor), _) => right.scale(factor),
                    (_, Some(factor)) => left.scale(factor),
                    _ => None,
                };
                scaled?.plus(-U64_MAX)
            }),
            _ => return,
        };
        let what = match op {
            BinaryOp::Subtract => "underflow",
            _ => "overflow",
        };
        out.push((goal, format!("u64 {what} in '{text}'")));
    }

    pub(super) fn assume_in_range(&self, state: &mut State, condition: &Expr) {
        let mut goals = Vec::new();
        self.range_goals(state, condition, &mut goals);
        state
            .facts
            .extend(goals.into_iter().filter_map(|(goal, _)| goal));
    }

    fn in_range(&mut self, state: &State, condition: &Expr) -> Result<(), String> {
        let mut goals = Vec::new();
        self.range_goals(state, condition, &mut goals);
        for (goal, what) in goals {
            if !goal.is_some_and(|goal| self.proves(&state.facts, &goal)) {
                return Err(what);
            }
        }
        Ok(())
    }

    pub(super) fn invariants_hold(&mut self, state: &State, when: &str, property: &'static str) {
        if self.quiet > 0 {
            return;
        }
        let program = self.program;
        for invariant in &program.invariants {
            if let Err(what) = self.in_range(state, &invariant.condition) {
                let text = self.names.expr(&invariant.condition);
                self.report(
                    property,
                    format!(
                        "invariant '{}' ({text}) may trap with {what} {when}",
                        invariant.name
                    ),
                    None,
                );
                continue;
            }
            match self.holds(state, &invariant.condition) {
                Ok(()) => *self.proved.entry(property).or_insert(0) += 1,
                Err(counterexample) => {
                    let text = self.names.expr(&invariant.condition);
                    self.report(
                        property,
                        format!(
                            "invariant '{}' ({text}) may not hold {when}",
                            invariant.name
                        ),
                        counterexample,
                    );
                }
            }
        }
    }
}
