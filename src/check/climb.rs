use std::collections::{BTreeMap, BTreeSet};

use crate::check::{Checked, Globals, bound, error, quoted};
use crate::ir::{self, Type};
use crate::run::mutation::Mutation;
use crate::syntax::ast::{
    self, Block, Climb, Expr, Item, Name, Param, Rule, Stmt, TypeExpr, Yield,
};
use crate::syntax::parser::{NESTING_DEPTH_MAX, Parser};
use crate::syntax::{Position, lexer};

pub const PREFIX: &str = "_climb_";
pub const CYCLE_TYPE: &str = "cycle";
pub const VERDICT_TYPE: &str = "verdict";
pub const END_TYPE: &str = "climb_end";
pub const VERDICT_KEPT: &str = "kept";
pub const CYCLE_FIELDS: usize = 14;
pub const CYCLE_BASELINE: usize = 3;
pub const CYCLE_CANDIDATE: usize = 4;
pub const CYCLE_BASELINE_SCORE: usize = 5;
pub const CYCLE_CANDIDATE_SCORE: usize = 6;
pub const CYCLE_WINS: usize = 7;
pub const CYCLE_TRIALS: usize = 8;
pub const CYCLE_COST: usize = 9;
pub const CYCLE_SPENT: usize = 10;
pub const CYCLE_VERDICT: usize = 11;
pub const CYCLE_GATE_FAILURES: usize = 13;
pub const CYCLE_INDEX: usize = 1;
pub const CYCLE_ROUND: usize = 0;
const ONE: &str = "_climb_one";
const TWO: &str = "_climb_two";
const SLOT: &str = "_climb_slot_";
const VERDICTS: [&str; 6] = ["failed", "kept", "worse", "gated", "regressed", "cancelled"];
const ENDS: [&str; 8] = [
    "running",
    "cycles",
    "budget",
    "wall",
    "failures",
    "breach",
    "cancelled",
    "full",
];
const RULE_NAMES: usize = 3;
const PURE_BUILTINS: [&str; 7] = [
    "len",
    "contains",
    "before",
    "after",
    "trim",
    "json_escape",
    "truncate_middle",
];

fn prefix(climb: &Climb) -> String {
    format!("{PREFIX}{}_", climb.name.text)
}

fn collect_block<'a>(block: &'a Block, out: &mut Vec<&'a Climb>) {
    for statement in &block.statements {
        collect(statement, out);
    }
}

fn collect<'a>(statement: &'a Stmt, out: &mut Vec<&'a Climb>) {
    match statement {
        Stmt::Climb(climb) => {
            out.push(climb);
            collect_block(&climb.propose.body, out);
            collect_block(&climb.measure.body, out);
        }
        Stmt::If {
            then, otherwise, ..
        } => {
            collect_block(then, out);
            if let Some(otherwise) = otherwise {
                collect_block(otherwise, out);
            }
        }
        Stmt::Match { arms, .. } => arms.iter().for_each(|arm| collect_block(&arm.body, out)),
        Stmt::Loop { body, .. } | Stmt::For { body, .. } | Stmt::Inlined { body, .. } => {
            collect_block(body, out);
        }
        _ => {}
    }
}

fn climbs(source: &ast::Program) -> Vec<&Climb> {
    let mut out = Vec::new();
    for item in &source.items {
        match item {
            Item::Handler(handler) => collect_block(&handler.body, &mut out),
            Item::Function(function) => collect_block(&function.body, &mut out),
            Item::Hook(hook) => collect_block(&hook.body, &mut out),
            _ => {}
        }
    }
    out
}

fn fragment(text: &str, at: Position) -> Vec<Stmt> {
    let mut lexemes = lexer::lex(text).unwrap_or_else(|diagnostic| {
        panic!("a climb's lowering lexes: {}: {text}", diagnostic.message)
    });
    lexemes.iter_mut().for_each(|lexeme| lexeme.at = at);
    Parser::new(&lexemes)
        .fragment()
        .unwrap_or_else(|diagnostic| {
            panic!("a climb's lowering parses: {}: {text}", diagnostic.message)
        })
}

fn declarations(climbs: &[&Climb]) -> String {
    let values = |values: &[&str]| {
        values
            .iter()
            .map(|value| quoted(value))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let mut text = format!(
        "harness {PREFIX}\nconst {ONE}: u64 = 1\nconst {TWO}: u64 = 2\ntype {VERDICT_TYPE} = {}\ntype {END_TYPE} = {}\ntype {CYCLE_TYPE} = {CYCLE_TYPE}(u64, u64, text, text, text, u64, u64, u64, u64, u64, u64, {VERDICT_TYPE}, text, u64)\n",
        values(&VERDICTS),
        values(&ENDS)
    );
    text.push_str("state {\n");
    for climb in climbs {
        let p = prefix(climb);
        let candidates = climb
            .candidates
            .as_ref()
            .map_or(ONE, |name| name.text.as_str());
        let mut fields: Vec<(String, String)> = Vec::new();
        let mut numbers = vec!["evaluated", "pair", "wins", "pairs", "gate_failures"];
        if climb.cost.is_some() {
            numbers.extend(["spent", "propose_spent", "trial_spent", "cost"]);
        }
        if climb.failures.is_some() {
            numbers.push("streak");
        }
        if climb.wall.is_some() {
            numbers.push("started");
        }
        if matches!(climb.rule, Rule::Best(_)) {
            numbers.extend(["base_best", "cand_best"]);
        }
        for name in numbers {
            fields.push((name.to_owned(), "u64".to_owned()));
        }
        for name in [
            "running", "breach", "scored", "ok", "kept", "gated", "accepted",
        ] {
            fields.push((name.to_owned(), "bool".to_owned()));
        }
        for name in ["breach_reason", "reason", "gate_reason"] {
            fields.push((name.to_owned(), "text".to_owned()));
        }
        fields.push(("verdict".to_owned(), VERDICT_TYPE.to_owned()));
        if climb.end.is_none() {
            fields.push(("end".to_owned(), END_TYPE.to_owned()));
        }
        fields.push((
            "proposals".to_owned(),
            format!("list<{CYCLE_TYPE}, {candidates}>"),
        ));
        for index in 0..climb.metrics.len() {
            for kind in ["value", "base", "cand"] {
                fields.push((format!("{kind}_{index}"), "u64".to_owned()));
            }
        }
        for (name, ty) in fields {
            text.push_str(&format!("  {p}{name}: {ty}\n"));
        }
    }
    text.push_str("}\n");
    text
}

fn reserved(source: &ast::Program) -> Checked<()> {
    let mut names: Vec<(&Name, &str)> = Vec::new();
    for item in &source.items {
        match item {
            Item::Const(constant) => names.push((&constant.name, "constant")),
            Item::Type(definition) => names.push((&definition.name, "type")),
            Item::Tool(tool) => names.push((&tool.name, "tool")),
            Item::Function(function) => names.push((&function.name, "fn")),
            Item::Invariant(invariant) => names.push((&invariant.name, "invariant")),
            Item::State(state) => names.extend(
                state
                    .fields
                    .iter()
                    .map(|field| (&field.name, "state field")),
            ),
            _ => {}
        }
    }
    match names.iter().find(|(name, _)| name.text.starts_with(PREFIX)) {
        Some((name, what)) => error(
            name.at,
            format!(
                "{what} '{}': names starting with '_' are reserved",
                name.text
            ),
        ),
        None => Ok(()),
    }
}

pub fn declare(source: &ast::Program) -> Checked<Option<ast::Program>> {
    reserved(source)?;
    let found = climbs(source);
    let Some(first) = found.first() else {
        return Ok(None);
    };
    let mut names: BTreeMap<&str, Position> = BTreeMap::new();
    for climb in &found {
        if let Some(previous) = names.insert(&climb.name.text, climb.name.at) {
            return error(
                climb.name.at,
                format!(
                    "climb '{}' is already declared at {previous}",
                    climb.name.text
                ),
            );
        }
    }
    let text = declarations(&found);
    let mut lexemes = lexer::lex(&text).expect("a climb's declarations lex");
    lexemes.iter_mut().for_each(|lexeme| lexeme.at = first.at);
    let declared = Parser::new(&lexemes)
        .program()
        .expect("a climb's declarations parse");
    let mut items = Vec::with_capacity(source.items.len() + declared.items.len());
    let mut hidden = Vec::new();
    for item in declared.items {
        match item {
            Item::State(state) => hidden = state.fields,
            other => items.push(other),
        }
    }
    let mut merged = false;
    for item in &source.items {
        match item {
            Item::State(state) if !merged => {
                let mut state = state.clone();
                state.fields.append(&mut hidden);
                items.push(Item::State(state));
                merged = true;
            }
            other => items.push(other.clone()),
        }
    }
    if !merged {
        items.push(Item::State(ast::State {
            at: first.at,
            fields: hidden,
        }));
    }
    Ok(Some(ast::Program {
        name: source.name.clone(),
        items,
    }))
}

#[derive(Clone, Copy)]
enum Side {
    Baseline,
    Candidate,
}

struct Plan<'c> {
    climb: &'c Climb,
    p: String,
    end: String,
    capacity: String,
    candidates: String,
    cancel: bool,
    mutation: Mutation,
}

impl Plan<'_> {
    fn better(&self, left: &str, right: &str) -> String {
        let p = &self.p;
        if self.climb.metrics[0].higher {
            format!("{p}{left} > {p}{right}")
        } else {
            format!("{p}{left} < {p}{right}")
        }
    }

    fn stop(&self, end: &str) -> String {
        format!(
            "{p}running = false {e} = {q}",
            p = self.p,
            e = self.end,
            q = quoted(end)
        )
    }

    fn statistics(&self) -> (String, String) {
        let p = &self.p;
        match self.climb.rule {
            Rule::Best(_) => (format!("{p}base_best"), format!("{p}cand_best")),
            _ => (format!("{p}base_0"), format!("{p}cand_0")),
        }
    }

    fn costed(&self, text: &str) -> String {
        if self.climb.cost.is_some() {
            text.replace("{p}", &self.p)
        } else {
            String::new()
        }
    }

    fn start(&self) -> String {
        let p = &self.p;
        let climb = self.climb;
        let mut text = self.costed("{p}spent = 0\n");
        if climb.failures.is_some() {
            text.push_str(&format!("{p}streak = 0\n"));
        }
        text.push_str(&format!("{p}running = true\n{} = \"running\"\n", self.end));
        if climb.from.is_some() {
            text.push_str(&format!(
                "if len({l}) == 0 {{ {o} = {SLOT}from }}\n",
                l = climb.ledger.text,
                o = climb.over.text
            ));
        }
        if climb.wall.is_some() {
            text.push_str(&format!("{p}started = now()\n"));
        }
        text.push_str(&format!("loop {p}cycle below {} {{\n", climb.cycles.text));
        if self.cancel {
            text.push_str(&format!(
                "if {p}running and cancelled {{ {} }}\n",
                self.stop("cancelled")
            ));
        }
        let mut guarded = format!("{}{}", self.round(), self.after_round());
        if let Some(cost) = &climb.cost
            && self.mutation != Mutation::ClimbBudgetUnchecked
        {
            guarded = format!(
                "if {pc} + {n} * {TWO} * {k} * {tc} > {b} {{ {stop} }} else {{ if {p}spent > {b} - ({pc} + {n} * {TWO} * {k} * {tc}) {{ {stop} }} else {{\n{guarded}}} }}\n",
                stop = self.stop("budget"),
                pc = cost.propose.text,
                n = self.candidates,
                k = climb.trials.text,
                tc = cost.trial.text,
                b = cost.budget.text
            );
        }
        text.push_str(&format!(
            "if {p}running {{\nif len({l}) + {n} > {c} {{ {} }} else {{\n{guarded}}}\n}}\n}}\n",
            self.stop("full"),
            l = climb.ledger.text,
            n = self.candidates,
            c = self.capacity
        ));
        text.push_str(&format!("if {p}running {{ {} }}\n", self.stop("cycles")));
        text
    }

    fn after_round(&self) -> String {
        let p = &self.p;
        let climb = self.climb;
        let mut text = String::new();
        if let Some(wall) = &climb.wall {
            text.push_str(&format!(
                "let {p}clock = now()\nif {p}running {{ if {p}clock >= {p}started {{ if {p}clock - {p}started >= {w} {{ {} }} }} }}\n",
                self.stop("wall"),
                w = wall.text
            ));
        }
        if let Some(failures) = &climb.failures {
            text.push_str(&format!(
                "if {p}running {{ if {p}streak >= {f} {{ {} }} }}\n",
                self.stop("failures"),
                f = failures.text
            ));
        }
        text
    }

    fn round(&self) -> String {
        let p = &self.p;
        let climb = self.climb;
        let (l, o) = (&climb.ledger.text, &climb.over.text);
        let (propose_cost, spent) = if climb.cost.is_some() {
            (format!("{p}propose_spent"), format!("{p}spent"))
        } else {
            ("0".to_owned(), "0".to_owned())
        };
        let streak = if climb.failures.is_some() {
            format!("{p}streak = {p}streak + 1\n")
        } else {
            String::new()
        };
        let mut text = format!(
            "clear {p}proposals\n{}{p}breach = false\n{p}breach_reason = \"\"\n{SLOT}propose()\n{}",
            self.costed("{p}propose_spent = 0\n"),
            self.costed("{p}spent = {p}spent + {p}propose_spent\n")
        );
        text.push_str(&format!(
            "if {p}breach {{ clear {p}proposals {} }}\n",
            self.stop("breach")
        ));
        text.push_str(&format!(
            "if len({p}proposals) == 0 {{\n{p}evaluated = {p}evaluated + 1\nif {p}breach {{ {p}reason = {p}breach_reason }} else {{ {p}reason = \"no candidate\" }}\npush {l} {CYCLE_TYPE}({p}cycle + 1, {p}evaluated, \"\", {o}, \"\", 0, 0, 0, 0, {propose_cost}, {spent}, \"failed\", {p}reason, 0)\n{streak}}} else {{\nfor {p}offer in {p}proposals {{\nmatch {p}offer {{\n{CYCLE_TYPE}(_, _, {p}hypothesis, _, {p}candidate, _, _, _, _, _, _, _, _, _) => {{\n{}}}\n}}\n}}\n}}\n",
            self.evaluate()
        ));
        text
    }

    fn evaluate(&self) -> String {
        let p = &self.p;
        let climb = self.climb;
        let (l, o) = (&climb.ledger.text, &climb.over.text);
        let mut text = String::new();
        for index in 0..climb.metrics.len() {
            text.push_str(&format!("{p}base_{index} = 0\n{p}cand_{index} = 0\n"));
        }
        if matches!(climb.rule, Rule::Best(_)) {
            text.push_str(&format!("{p}base_best = 0\n{p}cand_best = 0\n"));
        }
        text.push_str(&format!(
            "{p}pair = 0\n{p}wins = 0\n{p}pairs = 0\n{p}gate_failures = 0\n{p}ok = true\n{p}kept = false\n{p}verdict = \"failed\"\n{p}reason = \"\"\n{}",
            self.costed("{p}cost = {p}propose_spent\n{p}propose_spent = 0\n")
        ));
        text.push_str(&format!(
            "loop {p}trial below {k} {{\n",
            k = climb.trials.text
        ));
        for side in [Side::Baseline, Side::Candidate] {
            if self.cancel {
                text.push_str(&format!(
                    "if {p}ok and cancelled {{ {p}ok = false {p}verdict = \"cancelled\" {p}reason = \"cancelled during its trials\" {} }}\n",
                    self.stop("cancelled")
                ));
            }
            text.push_str(&format!("if {p}ok {{\n{}}}\n", self.trial(side)));
        }
        text.push_str("}\n");
        text.push_str(&format!("if {p}ok {{\n{}}}\n", self.rule()));
        let (base, cand) = self.statistics();
        let (cost, spent) = if climb.cost.is_some() {
            (format!("{p}cost"), format!("{p}spent"))
        } else {
            ("0".to_owned(), "0".to_owned())
        };
        let push = format!(
            "push {l} {CYCLE_TYPE}({p}cycle + 1, {p}evaluated, {p}hypothesis, {o}, {p}candidate, {base}, {cand}, {p}wins, {p}pairs, {cost}, {spent}, {p}verdict, {p}reason, {p}gate_failures)"
        );
        text.push_str(&format!("{p}evaluated = {p}evaluated + 1\n"));
        if self.mutation == Mutation::ClimbLedgerDropsLosses {
            text.push_str(&format!("if {p}kept {{ {push} }}\n"));
        } else {
            text.push_str(&format!("{push}\n"));
        }
        let lost = if self.mutation == Mutation::ClimbRevertSkipped {
            format!("{o} = {p}candidate\n")
        } else {
            String::new()
        };
        let (reset, streak) = if climb.failures.is_some() {
            (
                format!("{p}streak = 0\n"),
                format!("{p}streak = {p}streak + 1\n"),
            )
        } else {
            (String::new(), String::new())
        };
        text.push_str(&format!(
            "if {p}kept {{\n{o} = {p}candidate\n{reset}}} else {{\n{lost}{streak}}}\n"
        ));
        text
    }

    fn trial(&self, side: Side) -> String {
        let p = &self.p;
        let climb = self.climb;
        let mut inner = self.scored(side);
        for (index, metric) in climb.metrics.iter().enumerate().rev() {
            let reason = quoted(&format!("{} over {}", metric.name.text, metric.max.text));
            inner = format!(
                "if {p}value_{index} > {m} {{ {p}ok = false {p}reason = {reason} }} else {{\n{inner}}}\n",
                m = metric.max.text
            );
        }
        let (missing, slot) = match side {
            Side::Baseline => ("a baseline trial gave no score", "measure_baseline"),
            Side::Candidate => ("a candidate trial gave no score", "measure_candidate"),
        };
        let checked = format!(
            "if {p}scored {{\n{inner}}} else {{ {p}ok = false {p}reason = {} }}\n",
            quoted(missing)
        );
        let checked = if climb.cost.is_some() {
            format!(
                "if {p}breach {{ {p}ok = false {p}reason = {p}breach_reason {} }} else {{\n{checked}}}\n",
                self.stop("breach")
            )
        } else {
            checked
        };
        let charge = if self.mutation == Mutation::ClimbCostDoubleCounted {
            "{p}spent = {p}spent + {p}trial_spent + {p}trial_spent\n{p}cost = {p}cost + {p}trial_spent\n"
        } else {
            "{p}spent = {p}spent + {p}trial_spent\n{p}cost = {p}cost + {p}trial_spent\n"
        };
        format!(
            "{p}scored = false\n{}{SLOT}{slot}()\n{}{checked}",
            self.costed("{p}trial_spent = 0\n{p}breach = false\n"),
            self.costed(charge),
        )
    }

    fn scored(&self, side: Side) -> String {
        let p = &self.p;
        let climb = self.climb;
        let metrics = climb.metrics.len();
        let name = match side {
            Side::Baseline => "base",
            Side::Candidate => "cand",
        };
        let mut sums: String = (0..metrics)
            .map(|index| format!("{p}{name}_{index} = {p}{name}_{index} + {p}value_{index}\n"))
            .collect();
        if matches!(climb.rule, Rule::Best(_)) {
            sums.push_str(&format!(
                "if {p}trial == 0 or {} {{ {p}{name}_best = {p}value_0 }}\n",
                self.better("value_0", &format!("{name}_best"))
            ));
        }
        let accumulate = match side {
            Side::Baseline => format!("{sums}{p}pair = {p}value_0\n"),
            Side::Candidate => format!(
                "{sums}if {} {{ {p}wins = {p}wins + 1 }}\n{p}pairs = {p}pairs + 1\n",
                self.better("value_0", "pair")
            ),
        };
        if climb.gates.is_empty() {
            return accumulate;
        }
        let (failed, slot) = match side {
            Side::Baseline => (
                format!(
                    "{p}ok = false {p}reason = \"the baseline fails \" ++ {p}gate_reason {}",
                    self.stop("breach")
                ),
                "gates_baseline",
            ),
            Side::Candidate if self.mutation == Mutation::ClimbGateIgnored => (
                format!("{p}gate_failures = {p}gate_failures + 1\n{accumulate}"),
                "gates_candidate",
            ),
            Side::Candidate => (
                format!(
                    "{p}gate_failures = {p}gate_failures + 1 {p}ok = false {p}verdict = \"gated\" {p}reason = {p}gate_reason"
                ),
                "gates_candidate",
            ),
        };
        format!(
            "{p}gated = false\n{p}gate_reason = \"\"\n{SLOT}{slot}()\nif {p}gated {{ {failed} }} else {{\n{accumulate}}}\n"
        )
    }

    fn rule(&self) -> String {
        let p = &self.p;
        let climb = self.climb;
        let k = &climb.trials.text;
        let higher = climb.metrics[0].higher;
        let primary = &climb.metrics[0].name.text;
        let (condition, reason) = match &climb.rule {
            Rule::Mean(margin) => (
                if higher {
                    format!("{p}cand_0 > {p}base_0 + {k} * {}", margin.text)
                } else {
                    format!("{p}cand_0 + {k} * {} < {p}base_0", margin.text)
                },
                format!(
                    "mean {primary} does not beat the baseline's by more than {}",
                    margin.text
                ),
            ),
            Rule::Best(margin) => (
                if higher {
                    format!("{p}cand_best > {p}base_best + {}", margin.text)
                } else {
                    format!("{p}cand_best + {} < {p}base_best", margin.text)
                },
                format!(
                    "best {primary} does not beat the baseline's best by more than {}",
                    margin.text
                ),
            ),
            Rule::Sign(wins) => (
                format!("{p}wins >= {}", wins.text),
                format!("fewer than {} of {k} paired trials won", wins.text),
            ),
            Rule::When { .. } => (String::new(), "the accept rule does not hold".to_owned()),
        };
        let mut text = if matches!(climb.rule, Rule::When { .. }) {
            format!("{SLOT}accept()\n")
        } else if self.mutation == Mutation::ClimbAcceptAny {
            format!("{p}accepted = true\n")
        } else {
            format!("{p}accepted = {condition}\n")
        };
        text.push_str(&format!(
            "if {p}accepted {{ {p}kept = true {p}verdict = \"kept\" }} else {{ {p}verdict = \"worse\" {p}reason = {} }}\n",
            quoted(&reason)
        ));
        for (index, metric) in climb.metrics.iter().enumerate().skip(1) {
            let Some(slack) = &metric.within else {
                continue;
            };
            let beyond = if metric.higher {
                format!(
                    "{p}cand_{index} + {k} * {s} < {p}base_{index}",
                    s = slack.text
                )
            } else {
                format!(
                    "{p}cand_{index} > {p}base_{index} + {k} * {s}",
                    s = slack.text
                )
            };
            text.push_str(&format!(
                "if {p}kept {{ if {beyond} {{ {p}kept = false {p}verdict = \"regressed\" {p}reason = {} }} }}\n",
                quoted(&format!("{} beyond its tolerance {}", metric.name.text, slack.text))
            ));
        }
        text
    }

    fn rule_condition(&self) -> String {
        if self.mutation == Mutation::ClimbAcceptAny {
            "true".to_owned()
        } else {
            format!("{SLOT}rule")
        }
    }
}

struct Slots {
    statements: BTreeMap<String, Vec<Stmt>>,
    exprs: BTreeMap<String, Expr>,
}

fn fill_expr(expr: Expr, exprs: &BTreeMap<String, Expr>) -> Expr {
    match expr {
        Expr::Name(name) if name.text.starts_with(SLOT) => exprs
            .get(&name.text)
            .cloned()
            .unwrap_or_else(|| panic!("climb slot '{}' is filled", name.text)),
        Expr::Call {
            callee,
            args,
            mode,
            settings,
        } => Expr::Call {
            callee,
            args: args.into_iter().map(|arg| fill_expr(arg, exprs)).collect(),
            mode,
            settings,
        },
        Expr::Binary {
            at,
            op,
            left,
            right,
        } => Expr::Binary {
            at,
            op,
            left: Box::new(fill_expr(*left, exprs)),
            right: Box::new(fill_expr(*right, exprs)),
        },
        Expr::Not { at, operand } => Expr::Not {
            at,
            operand: Box::new(fill_expr(*operand, exprs)),
        },
        other => other,
    }
}

fn fill_block(block: Block, slots: &mut Slots) -> Block {
    Block {
        at: block.at,
        statements: fill(block.statements, slots),
    }
}

fn fill(statements: Vec<Stmt>, slots: &mut Slots) -> Vec<Stmt> {
    let mut out = Vec::with_capacity(statements.len());
    for statement in statements {
        match statement {
            Stmt::Call { callee, .. } if callee.text.starts_with(SLOT) => out.extend(
                slots
                    .statements
                    .remove(&callee.text)
                    .unwrap_or_else(|| panic!("climb slot '{}' is filled once", callee.text)),
            ),
            Stmt::Let { name, ty, value } => out.push(Stmt::Let {
                name,
                ty,
                value: fill_expr(value, &slots.exprs),
            }),
            Stmt::Assign { name, value } => out.push(Stmt::Assign {
                name,
                value: fill_expr(value, &slots.exprs),
            }),
            Stmt::Push { at, list, value } => out.push(Stmt::Push {
                at,
                list,
                value: fill_expr(value, &slots.exprs),
            }),
            Stmt::If {
                at,
                condition,
                then,
                otherwise,
            } => out.push(Stmt::If {
                at,
                condition: fill_expr(condition, &slots.exprs),
                then: fill_block(then, slots),
                otherwise: otherwise.map(|block| fill_block(block, slots)),
            }),
            Stmt::Match { at, subject, arms } => out.push(Stmt::Match {
                at,
                subject,
                arms: arms
                    .into_iter()
                    .map(|arm| ast::Arm {
                        pattern: arm.pattern,
                        body: fill_block(arm.body, slots),
                    })
                    .collect(),
            }),
            Stmt::Loop {
                at,
                name,
                bound,
                guard,
                body,
            } => out.push(Stmt::Loop {
                at,
                name,
                bound,
                guard,
                body: fill_block(body, slots),
            }),
            Stmt::For {
                at,
                name,
                list,
                parallel,
                body,
            } => out.push(Stmt::For {
                at,
                name,
                list,
                parallel,
                body: fill_block(body, slots),
            }),
            other => out.push(other),
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Propose,
    Measure,
}

struct Yields<'p> {
    plan: &'p Plan<'p>,
    phase: Phase,
}

impl Yields<'_> {
    fn block(&self, block: &Block) -> Checked<Block> {
        let mut statements = Vec::with_capacity(block.statements.len());
        for statement in &block.statements {
            statements.extend(self.statement(statement)?);
        }
        Ok(Block {
            at: block.at,
            statements,
        })
    }

    fn statement(&self, statement: &Stmt) -> Checked<Vec<Stmt>> {
        let name = &self.plan.climb.name.text;
        let expanded = match statement {
            Stmt::Yield { at, kind } => return self.expand(*at, kind),
            Stmt::Return { at, .. } => {
                return error(
                    *at,
                    format!(
                        "climb '{name}': a propose or measure block cannot return; the climb ends by its stop rules"
                    ),
                );
            }
            Stmt::Climb(inner) => {
                return error(
                    inner.at,
                    format!(
                        "climb '{}' cannot sit inside climb '{name}'",
                        inner.name.text
                    ),
                );
            }
            Stmt::If {
                at,
                condition,
                then,
                otherwise,
            } => Stmt::If {
                at: *at,
                condition: condition.clone(),
                then: self.block(then)?,
                otherwise: match otherwise {
                    Some(block) => Some(self.block(block)?),
                    None => None,
                },
            },
            Stmt::Match { at, subject, arms } => {
                let mut expanded = Vec::with_capacity(arms.len());
                for arm in arms {
                    expanded.push(ast::Arm {
                        pattern: arm.pattern.clone(),
                        body: self.block(&arm.body)?,
                    });
                }
                Stmt::Match {
                    at: *at,
                    subject: subject.clone(),
                    arms: expanded,
                }
            }
            Stmt::Loop {
                at,
                name,
                bound,
                guard,
                body,
            } => Stmt::Loop {
                at: *at,
                name: name.clone(),
                bound: bound.clone(),
                guard: guard.clone(),
                body: self.block(body)?,
            },
            Stmt::For {
                at,
                name,
                list,
                parallel,
                body,
            } => Stmt::For {
                at: *at,
                name: name.clone(),
                list: list.clone(),
                parallel: *parallel,
                body: self.block(body)?,
            },
            Stmt::Inlined {
                at,
                function,
                arguments,
                body,
                sees_caller,
            } => Stmt::Inlined {
                at: *at,
                function: function.clone(),
                arguments: arguments.clone(),
                body: self.block(body)?,
                sees_caller: *sees_caller,
            },
            other => other.clone(),
        };
        Ok(vec![expanded])
    }

    fn expand(&self, at: Position, kind: &Yield) -> Checked<Vec<Stmt>> {
        let plan = self.plan;
        let p = &plan.p;
        let climb = plan.climb;
        let name = &climb.name.text;
        let mut exprs = BTreeMap::new();
        let text = match (kind, self.phase) {
            (Yield::Candidate { value, hypothesis }, Phase::Propose) => {
                exprs.insert(format!("{SLOT}value"), value.clone());
                exprs.insert(format!("{SLOT}hypothesis"), hypothesis.clone());
                format!(
                    "if len({p}proposals) < {n} {{ push {p}proposals {CYCLE_TYPE}(0, 0, {SLOT}hypothesis, \"\", {SLOT}value, 0, 0, 0, 0, 0, 0, \"failed\", \"\", 0) }} else {{ {p}breach = true {p}breach_reason = {} }}\n",
                    quoted(&format!("more candidates than {}", plan.candidates)),
                    n = plan.candidates
                )
            }
            (Yield::Score(values), Phase::Measure) => {
                if values.len() != climb.metrics.len() {
                    return error(
                        at,
                        format!(
                            "climb '{name}' declares {} metric(s), so yield score takes {} value(s), found {}",
                            climb.metrics.len(),
                            climb.metrics.len(),
                            values.len()
                        ),
                    );
                }
                let mut text = String::new();
                for (index, value) in values.iter().enumerate() {
                    exprs.insert(format!("{SLOT}score_{index}"), value.clone());
                    text.push_str(&format!("{p}value_{index} = {SLOT}score_{index}\n"));
                }
                text.push_str(&format!("{p}scored = true\n"));
                text
            }
            (Yield::Cost(value), phase) => {
                let Some(cost) = &climb.cost else {
                    return error(
                        at,
                        format!("climb '{name}' has no cost clause, so nothing yields a cost"),
                    );
                };
                let (spent, max) = match phase {
                    Phase::Propose => (format!("{p}propose_spent"), &cost.propose.text),
                    Phase::Measure => (format!("{p}trial_spent"), &cost.trial.text),
                };
                exprs.insert(format!("{SLOT}cost"), value.clone());
                format!(
                    "let {yielded} = {SLOT}cost\nif {yielded} > {max} {{ {breach} }} else {{ if {spent} > {max} - {yielded} {{ {breach} }} else {{ {spent} = {spent} + {yielded} }} }}\n",
                    yielded = format!("{p}yielded_{}_{}", at.line, at.column),
                    breach = format!(
                        "{p}breach = true {p}breach_reason = {}",
                        quoted(&format!("a cost over {max}"))
                    )
                )
            }
            (Yield::Candidate { .. }, Phase::Measure) => {
                return error(
                    at,
                    format!("climb '{name}': yield candidate belongs in its propose block"),
                );
            }
            (Yield::Score(_), Phase::Propose) => {
                return error(
                    at,
                    format!("climb '{name}': yield score belongs in its measure block"),
                );
            }
        };
        let mut slots = Slots {
            statements: BTreeMap::new(),
            exprs,
        };
        Ok(fill(fragment(&text, at), &mut slots))
    }
}

fn pure(expr: &Expr, what: &str) -> Checked<()> {
    let mut pending = vec![expr];
    while let Some(expr) = pending.pop() {
        match expr {
            Expr::Call { callee, args, .. } => {
                if !PURE_BUILTINS.contains(&callee.text.as_str()) {
                    return error(
                        callee.at,
                        format!(
                            "{what} is a pure predicate: it cannot call '{}'",
                            callee.text
                        ),
                    );
                }
                pending.extend(args.iter());
            }
            Expr::Binary { left, right, .. } => {
                pending.push(left);
                pending.push(right);
            }
            Expr::Not { operand, .. } => pending.push(operand),
            Expr::Member { base, .. } => pending.push(base),
            Expr::Take { at, .. }
            | Expr::With { at, .. }
            | Expr::Index { at, .. }
            | Expr::Empty(at) => {
                return error(*at, format!("{what} is a pure predicate over the metrics"));
            }
            Expr::Number(..) | Expr::Text(..) | Expr::Bool(..) | Expr::Name(_) => {}
        }
    }
    Ok(())
}

fn render(expr: &Expr, depth: u32) -> String {
    assert!(
        depth <= NESTING_DEPTH_MAX,
        "the parser bounds an expression's depth"
    );
    match expr {
        Expr::Number(value, _) => value.to_string(),
        Expr::Text(text, _) => format!("{text:?}"),
        Expr::Bool(value, _) => value.to_string(),
        Expr::Name(name) => name.text.clone(),
        Expr::Member { base, field } => format!("{}.{}", render(base, depth + 1), field.text),
        Expr::Call { callee, args, .. } => format!(
            "{}({})",
            callee.text,
            args.iter()
                .map(|arg| render(arg, depth + 1))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Binary {
            op, left, right, ..
        } => format!(
            "{} {} {}",
            render(left, depth + 1),
            op.symbol(),
            render(right, depth + 1)
        ),
        Expr::Not { operand, .. } => format!("not {}", render(operand, depth + 1)),
        Expr::Take { events, .. } => format!("take {}", events.text),
        Expr::With { list, entry, .. } => format!(
            "{} with {}",
            render(list, depth + 1),
            render(entry, depth + 1)
        ),
        Expr::Index { base, index, .. } => {
            format!("{}[{}]", render(base, depth + 1), render(index, depth + 1))
        }
        Expr::Empty(_) => "[]".to_owned(),
    }
}

fn field<'g>(
    program: &'g ir::Program,
    globals: &Globals,
    name: &Name,
) -> Checked<(ir::FieldId, &'g Type)> {
    match globals.fields.get(&name.text) {
        Some(id) => Ok((*id, &program.state[*id].ty)),
        None => error(name.at, format!("'{}' is not a state field", name.text)),
    }
}

fn validate(
    program: &ir::Program,
    globals: &Globals,
    climb: &Climb,
) -> Checked<(ir::Climb, String)> {
    let name = &climb.name.text;
    let (over, over_ty) = field(program, globals, &climb.over)?;
    if *over_ty != Type::Text {
        return error(
            climb.over.at,
            format!(
                "climb '{name}' improves a text state field (a revision or a configuration): '{}' is not text",
                climb.over.text
            ),
        );
    }
    let cycle = globals.types[CYCLE_TYPE];
    let (ledger, ledger_ty) = field(program, globals, &climb.ledger)?;
    let capacity = match ledger_ty {
        Type::List { element, capacity } if **element == Type::Sum(cycle) => capacity.name.clone(),
        _ => {
            return error(
                climb.ledger.at,
                format!(
                    "climb '{name}' keeps its ledger in a list<{CYCLE_TYPE}, N> state field: '{}' is not one",
                    climb.ledger.text
                ),
            );
        }
    };
    if let Some(end) = &climb.end {
        let (_, end_ty) = field(program, globals, end)?;
        if *end_ty != Type::Sum(globals.types[END_TYPE]) {
            return error(
                end.at,
                format!(
                    "climb '{name}' writes why it ended to a {END_TYPE} state field: '{}' is not one",
                    end.text
                ),
            );
        }
    }
    let positive = |constant: &Name, what: &str| -> Checked<u64> {
        let value = bound(program, globals, constant, what)?.value;
        if value == 0 {
            return error(
                constant.at,
                format!("{what} of climb '{name}' is at least 1"),
            );
        }
        Ok(value)
    };
    let cycles = positive(&climb.cycles, "the cycles")?;
    let trials = positive(&climb.trials, "the trials")?;
    let candidates = match &climb.candidates {
        Some(constant) => positive(constant, "the candidates")?,
        None => 1,
    };
    let budget = match &climb.cost {
        Some(cost) => {
            bound(program, globals, &cost.propose, "a proposal's cost")?;
            bound(program, globals, &cost.trial, "a trial's cost")?;
            Some(bound(program, globals, &cost.budget, "a climb's cost budget")?.value)
        }
        None => None,
    };
    for constant in climb.wall.iter().chain(climb.failures.iter()) {
        positive(constant, "a stop rule")?;
    }
    validate_metrics(program, globals, climb)?;
    let rule = match &climb.rule {
        Rule::Mean(margin) => {
            ir::ClimbRule::Mean(bound(program, globals, margin, "a margin")?.value)
        }
        Rule::Best(margin) => {
            ir::ClimbRule::Best(bound(program, globals, margin, "a margin")?.value)
        }
        Rule::Sign(wins) => {
            let needed = positive(wins, "the wins a sign rule needs")?;
            if needed > trials {
                return error(
                    wins.at,
                    format!("climb '{name}' needs {needed} wins of {trials} trials"),
                );
            }
            ir::ClimbRule::Sign(needed)
        }
        Rule::When { names, condition } => {
            if names.len() != RULE_NAMES {
                return error(
                    condition.at(),
                    format!(
                        "accept when binds (baseline, candidate, wins): {RULE_NAMES} names, found {}",
                        names.len()
                    ),
                );
            }
            pure(condition, "an accept rule")?;
            ir::ClimbRule::When
        }
    };
    let evaluated = globals.fields[&format!("{}evaluated", prefix(climb))];
    Ok((
        ir::Climb {
            name: name.clone(),
            over,
            ledger,
            evaluated,
            cycles,
            candidates,
            trials,
            budget,
            higher: climb.metrics[0].higher,
            rule,
        },
        capacity,
    ))
}

fn validate_metrics(program: &ir::Program, globals: &Globals, climb: &Climb) -> Checked<()> {
    let mut seen = BTreeSet::new();
    for (index, metric) in climb.metrics.iter().enumerate() {
        if !seen.insert(metric.name.text.as_str()) {
            return error(
                metric.name.at,
                format!("metric '{}' appears twice", metric.name.text),
            );
        }
        bound(program, globals, &metric.max, "a metric's largest value")?;
        if let Some(within) = &metric.within {
            if index == 0 {
                return error(
                    within.at,
                    format!(
                        "metric '{}' is the primary metric, which the accept rule judges: only a secondary metric has a tolerance",
                        metric.name.text
                    ),
                );
            }
            bound(program, globals, within, "a metric's tolerance")?;
        }
    }
    for gate in &climb.gates {
        pure(gate, "a gate")?;
    }
    Ok(())
}

fn lower(
    climb: &Climb,
    capacity: String,
    program: &ir::Program,
    mutation: Mutation,
) -> Checked<Vec<Stmt>> {
    let plan = Plan {
        climb,
        p: prefix(climb),
        end: climb
            .end
            .as_ref()
            .map_or_else(|| format!("{}end", prefix(climb)), |end| end.text.clone()),
        capacity,
        candidates: climb
            .candidates
            .as_ref()
            .map_or_else(|| ONE.to_owned(), |name| name.text.clone()),
        cancel: program.events.cancel,
        mutation,
    };
    let text_ty = || {
        TypeExpr::Named(Name {
            text: "text".to_owned(),
            at: climb.at,
        })
    };
    let u64_ty = || {
        TypeExpr::Named(Name {
            text: "u64".to_owned(),
            at: climb.at,
        })
    };
    let named = |text: String| Expr::Name(Name { text, at: climb.at });
    let scoped = |what: &str, arguments: Vec<(Param, Expr)>, body: Block| Stmt::Inlined {
        at: body.at,
        function: Name {
            text: format!("climb {} {what}", climb.name.text),
            at: climb.at,
        },
        arguments,
        body,
        sees_caller: true,
    };
    let propose = Yields {
        plan: &plan,
        phase: Phase::Propose,
    }
    .block(&climb.propose.body)?;
    let measure = Yields {
        plan: &plan,
        phase: Phase::Measure,
    }
    .block(&climb.measure.body)?;
    let p = &plan.p;
    let metrics: Vec<(Param, Expr)> = climb
        .metrics
        .iter()
        .enumerate()
        .map(|(index, metric)| {
            (
                Param {
                    name: metric.name.clone(),
                    ty: u64_ty(),
                },
                named(format!("{p}value_{index}")),
            )
        })
        .collect();
    let mut gates = Vec::with_capacity(climb.gates.len());
    for (index, gate) in climb.gates.iter().enumerate() {
        let reason = quoted(&format!("gate {} ({})", index + 1, render(gate, 0)));
        let text = format!(
            "if not {SLOT}gate {{ if not {p}gated {{ {p}gated = true {p}gate_reason = {reason} }} }}\n"
        );
        let mut slots = Slots {
            statements: BTreeMap::new(),
            exprs: BTreeMap::from([(format!("{SLOT}gate"), gate.clone())]),
        };
        let body = fill(fragment(&text, gate.at()), &mut slots);
        gates.push(scoped(
            "gate",
            metrics.clone(),
            Block {
                at: gate.at(),
                statements: body,
            },
        ));
    }
    let mut statements = BTreeMap::from([(
        format!("{SLOT}propose"),
        vec![scoped(
            "propose",
            vec![(
                Param {
                    name: climb.propose.param.clone(),
                    ty: text_ty(),
                },
                named(climb.over.text.clone()),
            )],
            propose,
        )],
    )]);
    for (side, subject) in [
        ("baseline", climb.over.text.clone()),
        ("candidate", format!("{p}candidate")),
    ] {
        statements.insert(
            format!("{SLOT}measure_{side}"),
            vec![scoped(
                "measure",
                vec![(
                    Param {
                        name: climb.measure.param.clone(),
                        ty: text_ty(),
                    },
                    named(subject),
                )],
                measure.clone(),
            )],
        );
        if !gates.is_empty() {
            statements.insert(format!("{SLOT}gates_{side}"), gates.clone());
        }
    }
    let mut exprs = BTreeMap::new();
    if let Some(from) = &climb.from {
        exprs.insert(format!("{SLOT}from"), from.clone());
    }
    if let Rule::When { names, condition } = &climb.rule {
        let (base, cand) = plan.statistics();
        let arguments = [base, cand, format!("{p}wins")]
            .into_iter()
            .zip(names)
            .map(|(value, name)| {
                (
                    Param {
                        name: name.clone(),
                        ty: u64_ty(),
                    },
                    named(value),
                )
            })
            .collect();
        let text = format!("{p}accepted = {}\n", plan.rule_condition());
        let mut slots = Slots {
            statements: BTreeMap::new(),
            exprs: BTreeMap::from([(format!("{SLOT}rule"), condition.clone())]),
        };
        let body = fill(fragment(&text, condition.at()), &mut slots);
        statements.insert(
            format!("{SLOT}accept"),
            vec![scoped(
                "accept",
                arguments,
                Block {
                    at: condition.at(),
                    statements: body,
                },
            )],
        );
    }
    let mut slots = Slots { statements, exprs };
    let lowered = fill(fragment(&plan.start(), climb.at), &mut slots);
    assert!(
        slots.statements.is_empty(),
        "every statement slot of a climb's lowering is filled"
    );
    Ok(lowered)
}

struct Lowering<'a> {
    program: &'a ir::Program,
    globals: &'a Globals,
    mutation: Mutation,
    found: BTreeMap<String, ir::Climb>,
    protected: BTreeMap<String, (String, bool)>,
}

impl Lowering<'_> {
    fn block(&mut self, block: &Block) -> Checked<Block> {
        let mut statements = Vec::with_capacity(block.statements.len());
        for statement in &block.statements {
            statements.extend(self.statement(statement)?);
        }
        Ok(Block {
            at: block.at,
            statements,
        })
    }

    fn statement(&mut self, statement: &Stmt) -> Checked<Vec<Stmt>> {
        let lowered = match statement {
            Stmt::Climb(climb) => {
                let (checked, capacity) = validate(self.program, self.globals, climb)?;
                self.found.entry(checked.name.clone()).or_insert(checked);
                return lower(climb, capacity, self.program, self.mutation);
            }
            Stmt::Yield { at, .. } => {
                return error(*at, "yield belongs in a climb's propose or measure block");
            }
            Stmt::If {
                at,
                condition,
                then,
                otherwise,
            } => Stmt::If {
                at: *at,
                condition: condition.clone(),
                then: self.block(then)?,
                otherwise: match otherwise {
                    Some(block) => Some(self.block(block)?),
                    None => None,
                },
            },
            Stmt::Match { at, subject, arms } => {
                let mut lowered = Vec::with_capacity(arms.len());
                for arm in arms {
                    lowered.push(ast::Arm {
                        pattern: arm.pattern.clone(),
                        body: self.block(&arm.body)?,
                    });
                }
                Stmt::Match {
                    at: *at,
                    subject: subject.clone(),
                    arms: lowered,
                }
            }
            Stmt::Loop {
                at,
                name,
                bound,
                guard,
                body,
            } => Stmt::Loop {
                at: *at,
                name: name.clone(),
                bound: bound.clone(),
                guard: guard.clone(),
                body: self.block(body)?,
            },
            Stmt::For {
                at,
                name,
                list,
                parallel,
                body,
            } => Stmt::For {
                at: *at,
                name: name.clone(),
                list: list.clone(),
                parallel: *parallel,
                body: self.block(body)?,
            },
            Stmt::Inlined {
                at,
                function,
                arguments,
                body,
                sees_caller,
            } => Stmt::Inlined {
                at: *at,
                function: function.clone(),
                arguments: arguments.clone(),
                body: self.block(body)?,
                sees_caller: *sees_caller,
            },
            other => other.clone(),
        };
        Ok(vec![lowered])
    }

    fn writes(&self, block: &Block) -> Checked<()> {
        for statement in &block.statements {
            let written = match statement {
                Stmt::Assign { name, .. } => Some((name, false)),
                Stmt::Push { list, .. }
                | Stmt::Clear { list, .. }
                | Stmt::Keep { list, .. }
                | Stmt::KeepBlobs { list, .. }
                | Stmt::Rewrite { list, .. } => Some((list, true)),
                _ => None,
            };
            if let Some((name, listed)) = written
                && let Some((climb, ledger)) = self.protected.get(&name.text)
            {
                let what = match (ledger, listed) {
                    (true, _) => "is the append-only ledger of",
                    (false, _) => "holds the accepted baseline or the end of",
                };
                return error(
                    name.at,
                    format!(
                        "'{}' {what} climb '{climb}': only the climb writes it (its from clause sets the first baseline)",
                        name.text
                    ),
                );
            }
            self.writes_within(statement)?;
        }
        Ok(())
    }

    fn writes_within(&self, statement: &Stmt) -> Checked<()> {
        match statement {
            Stmt::If {
                then, otherwise, ..
            } => {
                self.writes(then)?;
                if let Some(otherwise) = otherwise {
                    self.writes(otherwise)?;
                }
            }
            Stmt::Match { arms, .. } => {
                for arm in arms {
                    self.writes(&arm.body)?;
                }
            }
            Stmt::Loop { body, .. } | Stmt::For { body, .. } | Stmt::Inlined { body, .. } => {
                self.writes(body)?;
            }
            Stmt::Climb(climb) => {
                self.writes(&climb.propose.body)?;
                self.writes(&climb.measure.body)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn protect(source: &ast::Program) -> BTreeMap<String, (String, bool)> {
    let mut protected = BTreeMap::new();
    for climb in climbs(source) {
        let name = climb.name.text.clone();
        protected.insert(climb.over.text.clone(), (name.clone(), false));
        protected.insert(climb.ledger.text.clone(), (name.clone(), true));
        if let Some(end) = &climb.end {
            protected.insert(end.text.clone(), (name, false));
        }
    }
    protected
}

pub fn expand(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &Globals,
    mutation: Mutation,
) -> Checked<Option<ast::Program>> {
    let found = climbs(source);
    if found.is_empty() {
        return Ok(None);
    }
    let resets: Vec<String> = found
        .iter()
        .map(|climb| format!("{}evaluated", prefix(climb)))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut lowering = Lowering {
        program,
        globals,
        mutation,
        found: BTreeMap::new(),
        protected: protect(source),
    };
    let mut items = Vec::with_capacity(source.items.len());
    for item in &source.items {
        match item {
            Item::Handler(handler) => {
                lowering.writes(&handler.body)?;
                let mut handler = handler.clone();
                let mut inner = Vec::new();
                collect_block(&handler.body, &mut inner);
                if handler.side
                    && let Some(climb) = inner.first()
                {
                    return error(
                        climb.at,
                        format!(
                            "climb '{}' writes state, so it cannot run in a side handler",
                            climb.name.text
                        ),
                    );
                }
                handler.body = lowering.block(&handler.body)?;
                if !handler.side {
                    let at = handler.name.at;
                    let prologue = resets.iter().map(|field| Stmt::Assign {
                        name: Name {
                            text: field.clone(),
                            at,
                        },
                        value: Expr::Number(0, at),
                    });
                    handler.body.statements.splice(0..0, prologue);
                }
                items.push(Item::Handler(handler));
            }
            Item::Hook(hook) => {
                lowering.writes(&hook.body)?;
                let mut inner = Vec::new();
                collect_block(&hook.body, &mut inner);
                if let Some(climb) = inner.first() {
                    return error(
                        climb.at,
                        format!(
                            "climb '{}' runs in a handler, not in '{}'",
                            climb.name.text,
                            hook.kind.describe()
                        ),
                    );
                }
                items.push(item.clone());
            }
            other => items.push(other.clone()),
        }
    }
    let found = std::mem::take(&mut lowering.found);
    program.climbs = found.into_values().collect();
    Ok(Some(ast::Program {
        name: source.name.clone(),
        items,
    }))
}
