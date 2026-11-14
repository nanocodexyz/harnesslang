use crate::ir::{BinaryOp, Expr, Type};
use crate::prove::Severity;
use crate::prove::arith::{Alternatives, Prover, State, U64_MAX, product};
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
            Expr::Construct { fields, .. } => {
                for field in fields {
                    self.value(state, field);
                }
                None
            }
            Expr::Len(list) => {
                self.value(state, list);
                self.linear_of(state, value)
            }
            Expr::U64(_) | Expr::Constant(_) | Expr::Local(_) | Expr::Field(_) => self.linear_of(state, value),
            Expr::Bool(_) | Expr::Text(_) => None,
        }
    }

    fn add(&mut self, state: &mut State, left: &Linear, right: &Linear, text: String) -> Option<Linear> {
        let sum = left.add(right)?;
        let goal = sum.plus(-U64_MAX)?;
        self.obligation(state, goal, Severity::Error, "no_overflow", format!("u64 overflow in '{text}' is possible"));
        Some(sum)
    }

    fn subtract(&mut self, state: &mut State, left: &Linear, right: &Linear, text: String) -> Option<Linear> {
        let difference = left.subtract(right)?;
        let goal = right.subtract(left)?;
        self.obligation(state, goal, Severity::Error, "no_overflow", format!("u64 underflow in '{text}' is possible"));
        Some(difference)
    }

    fn multiply(&mut self, state: &mut State, left: &Linear, right: &Linear, text: String) -> Linear {
        let message = format!("u64 overflow in '{text}' is possible");
        let scaled = match (left.as_constant(), right.as_constant()) {
            (Some(factor), _) => right.scale(factor),
            (_, Some(factor)) => left.scale(factor),
            _ => None,
        };
        if let Some(scaled) = scaled
            && let Some(goal) = scaled.plus(-U64_MAX)
        {
            self.obligation(state, goal, Severity::Error, "no_overflow", message);
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
                self.report(Severity::Error, "no_overflow", message, Some(counterexample));
                Linear::var(self.fresh(state, &product_name, U64_MAX))
            }
        }
    }

    fn comparison(&self, state: &State, op: BinaryOp, left: &Expr, right: &Expr, truth: bool) -> Option<Alternatives> {
        let left = self.linear_of(state, left)?;
        let right = self.linear_of(state, right)?;
        let below = |low: &Linear, high: &Linear| low.subtract(high)?.plus(1);
        let at_most = |low: &Linear, high: &Linear| low.subtract(high);
        let alternatives = match (op, truth) {
            (BinaryOp::Less, true) | (BinaryOp::GreaterEqual, false) => vec![vec![below(&left, &right)?]],
            (BinaryOp::Less, false) | (BinaryOp::GreaterEqual, true) => vec![vec![at_most(&right, &left)?]],
            (BinaryOp::LessEqual, true) | (BinaryOp::Greater, false) => vec![vec![at_most(&left, &right)?]],
            (BinaryOp::LessEqual, false) | (BinaryOp::Greater, true) => vec![vec![below(&right, &left)?]],
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

    pub(super) fn goals(&self, state: &State, condition: &Expr, truth: bool) -> Option<Alternatives> {
        match condition {
            Expr::Bool(value) => Some(if *value == truth { vec![Vec::new()] } else { Vec::new() }),
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
        match self.goals(&state, condition, truth) {
            Some(alternatives) if alternatives.is_empty() => state.facts.push(Linear::constant(1)),
            Some(mut alternatives) if alternatives.len() == 1 => {
                state.facts.extend(alternatives.remove(0));
            }
            _ => {}
        }
        state
    }

    pub(super) fn holds(&mut self, state: &State, condition: &Expr) -> Result<(), Option<String>> {
        let Some(alternatives) = self.goals(state, condition, true) else {
            return Err(Some("the prover reasons only about u64 comparisons joined by and, or and not".to_owned()));
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

    pub(super) fn invariants_hold(&mut self, state: &State, when: &str, property: &'static str) {
        if self.quiet > 0 {
            return;
        }
        let program = self.program;
        for invariant in &program.invariants {
            match self.holds(state, &invariant.condition) {
                Ok(()) => *self.proved.entry(property).or_insert(0) += 1,
                Err(counterexample) => {
                    let text = self.names.expr(&invariant.condition);
                    self.report(
                        Severity::Error,
                        property,
                        format!("invariant '{}' ({text}) may not hold {when}", invariant.name),
                        counterexample,
                    );
                }
            }
        }
    }
}
