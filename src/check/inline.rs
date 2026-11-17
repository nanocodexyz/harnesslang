use std::collections::{BTreeMap, BTreeSet};

use crate::check::{
    ALLOW, BUILTIN_FUNCTIONS, CANCELLED_READ, Checked, DENY, Globals, RESPOND, WILDCARD, claim,
    error, variant_names,
};
use crate::ir;
use crate::syntax::Position;
use crate::syntax::ast::{
    self, Arm, Block, Climb, Expr, Function, HookKind, Item, Name, Pattern, Rule, Stmt, Yield,
};
use crate::syntax::parser::NESTING_DEPTH_MAX;

pub const STATEMENTS_PER_HANDLER_MAX: u64 = 65_536;

struct Expansion {
    body: Block,
    statements: u64,
    depth: u32,
}

#[derive(Clone, Copy)]
enum Owner<'a> {
    Handler(&'a str),
    Function(&'a str),
    Invariant(&'a str),
    Hook(HookKind),
}

impl Owner<'_> {
    fn describe(self) -> String {
        match self {
            Self::Handler(name) => format!("handler '{name}'"),
            Self::Function(name) => format!("fn '{name}'"),
            Self::Invariant(name) => format!("invariant '{name}'"),
            Self::Hook(kind) => format!("'{}'", kind.describe()),
        }
    }
}

struct Walk<'a> {
    owner: Owner<'a>,
    scopes: Vec<Vec<String>>,
    statements: u64,
    depth: u32,
    deepest: u32,
}

impl<'a> Walk<'a> {
    fn new(owner: Owner<'a>) -> Self {
        Self {
            owner,
            scopes: vec![Vec::new()],
            statements: 0,
            depth: 0,
            deepest: 0,
        }
    }

    fn visible(&self, name: &str) -> bool {
        self.scopes.iter().flatten().any(|bound| bound == name)
    }

    fn bind(&mut self, name: &Name) -> Checked<()> {
        if name.text == WILDCARD {
            return Ok(());
        }
        if name.text.starts_with(crate::check::climb::PREFIX) {
            return error(
                name.at,
                format!("'{}': names starting with '_' are reserved", name.text),
            );
        }
        self.scopes
            .last_mut()
            .expect("a walk always has a scope")
            .push(name.text.clone());
        Ok(())
    }

    fn scoped<T>(
        &mut self,
        names: &[Name],
        body: impl FnOnce(&mut Self) -> Checked<T>,
    ) -> Checked<T> {
        self.scopes.push(Vec::new());
        let result = names
            .iter()
            .try_for_each(|name| self.bind(name))
            .and_then(|()| body(self));
        self.scopes.pop().expect("every pushed scope is popped");
        result
    }

    fn reference(&self, declared: &BTreeSet<String>, name: &Name) -> Checked<()> {
        if name.text.starts_with(WILDCARD) {
            return error(
                name.at,
                format!("'{}': names starting with '_' are reserved", name.text),
            );
        }
        if self.visible(&name.text) {
            return Ok(());
        }
        match self.owner {
            Owner::Function(function) if !declared.contains(&name.text) => error(
                name.at,
                format!(
                    "fn '{function}' cannot see '{}': a fn sees its parameters, its own bindings and the harness's declarations",
                    name.text
                ),
            ),
            Owner::Hook(kind) if !declared.contains(&name.text) => error(
                name.at,
                format!(
                    "'{}' cannot see '{}': a hook sees its parameters, its own bindings and the harness's declarations",
                    kind.describe(),
                    name.text
                ),
            ),
            _ => Ok(()),
        }
    }

    fn count(&mut self, at: Position, statements: u64) -> Checked<()> {
        self.statements = self
            .statements
            .checked_add(statements)
            .expect("statements are counted below their bound, far from u64's end");
        if self.statements > STATEMENTS_PER_HANDLER_MAX {
            return error(
                at,
                format!(
                    "{} has more than statements_per_handler_max ({STATEMENTS_PER_HANDLER_MAX}) statements once its fn calls are inlined",
                    self.owner.describe()
                ),
            );
        }
        Ok(())
    }
}

struct Inliner<'a> {
    functions: BTreeMap<&'a str, &'a Function>,
    declared: BTreeSet<String>,
    expanded: BTreeMap<&'a str, Expansion>,
    called: BTreeSet<&'a str>,
    stack: Vec<(&'a str, Position)>,
}

pub fn expand(
    source: &ast::Program,
    program: &ir::Program,
    globals: &Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<ast::Program> {
    let variants = variant_names(program);
    let mut inliner = Inliner {
        functions: BTreeMap::new(),
        declared: declared(globals, &variants, program),
        expanded: BTreeMap::new(),
        called: BTreeSet::new(),
        stack: Vec::new(),
    };
    let functions: Vec<&Function> = source
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) if function.returns.is_none() => Some(function),
            _ => None,
        })
        .collect();
    for function in &functions {
        inliner.declare(function, values, &variants)?;
    }
    for function in &functions {
        inliner.function(function, function.name.at)?;
    }
    let mut items = Vec::with_capacity(source.items.len());
    for item in &source.items {
        match item {
            Item::Function(_) => {}
            Item::Handler(handler) => items.push(Item::Handler(inliner.handler(handler)?)),
            Item::Hook(hook) => items.push(Item::Hook(inliner.hook(hook)?)),
            Item::Invariant(invariant) => {
                let mut walk = Walk::new(Owner::Invariant(&invariant.name.text));
                inliner.expr(&mut walk, &invariant.condition)?;
                items.push(item.clone());
            }
            other => items.push(other.clone()),
        }
    }
    assert!(inliner.stack.is_empty(), "every fn expansion is finished");
    if let Some(unused) = functions
        .iter()
        .find(|function| !inliner.called.contains(function.name.text.as_str()))
    {
        return error(
            unused.name.at,
            format!("fn '{}' is never called", unused.name.text),
        );
    }
    Ok(ast::Program {
        name: source.name.clone(),
        items,
    })
}

fn declared(
    globals: &Globals,
    variants: &BTreeSet<String>,
    program: &ir::Program,
) -> BTreeSet<String> {
    let mut names = variants.clone();
    if program.events.cancel {
        names.insert(CANCELLED_READ.to_owned());
    }
    names.extend(globals.fields.keys().cloned());
    names.extend(globals.constants.keys().cloned());
    names.extend(globals.tools.keys().cloned());
    names.extend(globals.listings.keys().cloned());
    names.extend(globals.model.iter().cloned());
    names.extend(globals.functions.keys().cloned());
    names.extend(BUILTIN_FUNCTIONS.iter().map(|name| (*name).to_owned()));
    names
}

impl<'a> Inliner<'a> {
    fn declare(
        &mut self,
        function: &'a Function,
        values: &mut BTreeMap<String, Position>,
        variants: &BTreeSet<String>,
    ) -> Checked<()> {
        claim(values, &function.name, "fn")?;
        if variants.contains(&function.name.text) {
            return error(
                function.name.at,
                format!("fn '{}' has the name of a variant", function.name.text),
            );
        }
        let mut seen = BTreeSet::new();
        for param in &function.params {
            if !seen.insert(param.name.text.as_str()) {
                return error(
                    param.name.at,
                    format!("parameter '{}' appears twice", param.name.text),
                );
            }
        }
        let previous = self.functions.insert(&function.name.text, function);
        assert!(previous.is_none(), "claim refuses a second fn of one name");
        Ok(())
    }

    fn function(&mut self, function: &'a Function, at: Position) -> Checked<()> {
        let name = function.name.text.as_str();
        if self.expanded.contains_key(name) {
            return Ok(());
        }
        if let Some(start) = self.stack.iter().position(|(entered, _)| *entered == name) {
            let mut cycle: Vec<&str> = self.stack[start..]
                .iter()
                .map(|(entered, _)| *entered)
                .collect();
            cycle.push(name);
            return error(
                at,
                format!(
                    "fn '{name}' is recursive: {}; a fn is inlined where it is called, so it cannot call itself, directly or through another fn",
                    cycle.join(" -> ")
                ),
            );
        }
        if self.stack.len() >= NESTING_DEPTH_MAX as usize {
            return error(
                at,
                format!("calls of fns nest deeper than nesting_depth_max ({NESTING_DEPTH_MAX})"),
            );
        }
        self.stack.push((name, at));
        let mut walk = Walk::new(Owner::Function(name));
        function
            .params
            .iter()
            .try_for_each(|param| walk.bind(&param.name))?;
        let body = self.block(&mut walk, &function.body)?;
        let finished = self.stack.pop();
        assert_eq!(
            finished,
            Some((name, at)),
            "fn expansions finish in the order they start"
        );
        assert!(walk.deepest >= 1, "a fn body is one block deep at least");
        self.expanded.insert(
            name,
            Expansion {
                body,
                statements: walk.statements,
                depth: walk.deepest,
            },
        );
        Ok(())
    }

    fn handler(&mut self, handler: &'a ast::Handler) -> Checked<ast::Handler> {
        let mut walk = Walk::new(Owner::Handler(&handler.name.text));
        handler
            .params
            .iter()
            .try_for_each(|param| walk.bind(&param.name))?;
        if let Some(requires) = &handler.requires {
            self.expr(&mut walk, requires)?;
        }
        let body = self.block(&mut walk, &handler.body)?;
        Ok(ast::Handler {
            name: handler.name.clone(),
            params: handler.params.clone(),
            returns: handler.returns.clone(),
            requires: handler.requires.clone(),
            body,
            ends_session: handler.ends_session,
            side: handler.side,
            idle: handler.idle.clone(),
        })
    }

    fn hook(&mut self, hook: &'a ast::Hook) -> Checked<ast::Hook> {
        let mut walk = Walk::new(Owner::Hook(hook.kind));
        hook.params
            .iter()
            .try_for_each(|param| walk.bind(&param.name))?;
        let body = self.block(&mut walk, &hook.body)?;
        Ok(ast::Hook {
            kind: hook.kind,
            at: hook.at,
            params: hook.params.clone(),
            body,
        })
    }

    fn verdict(
        &mut self,
        walk: &mut Walk<'a>,
        callee: &'a Name,
        args: &'a [Expr],
    ) -> Checked<Stmt> {
        let allowed = match walk.owner {
            Owner::Hook(HookKind::BeforeTool) => true,
            Owner::Hook(HookKind::AfterTool) => callee.text == DENY,
            _ => false,
        };
        if !allowed {
            let place = if callee.text == DENY {
                "a 'before tool' or 'after tool' block"
            } else if callee.text == RESPOND {
                "a 'before tool' block, which may answer a call instead of its tool"
            } else {
                "a 'before tool' block, which may rewrite a call before it runs"
            };
            return error(
                callee.at,
                format!(
                    "'{}' belongs in {place}, not in {}",
                    callee.text,
                    walk.owner.describe()
                ),
            );
        }
        if args.len() != 1 {
            return error(
                callee.at,
                format!("{} takes 1 argument(s), found {}", callee.text, args.len()),
            );
        }
        for arg in args {
            self.expr(walk, arg)?;
        }
        Ok(Stmt::Call {
            callee: callee.clone(),
            args: args.to_vec(),
        })
    }

    fn block(&mut self, walk: &mut Walk<'a>, block: &'a Block) -> Checked<Block> {
        walk.depth += 1;
        walk.deepest = walk.deepest.max(walk.depth);
        let statements = walk.scoped(&[], |walk| {
            let mut statements = Vec::with_capacity(block.statements.len());
            for statement in &block.statements {
                statements.push(self.statement(walk, statement)?);
            }
            Ok(statements)
        })?;
        walk.depth -= 1;
        Ok(Block {
            at: block.at,
            statements,
        })
    }

    fn statement(&mut self, walk: &mut Walk<'a>, statement: &'a Stmt) -> Checked<Stmt> {
        walk.count(super::handler::statement_at(statement), 1)?;
        match statement {
            Stmt::Let { name, value, .. } | Stmt::Var { name, value, .. } => {
                self.expr(walk, value)?;
                walk.bind(name)?;
                Ok(statement.clone())
            }
            Stmt::SetIndex {
                list, index, value, ..
            } => {
                walk.reference(&self.declared, list)?;
                self.expr(walk, index)?;
                self.expr(walk, value)?;
                Ok(statement.clone())
            }
            Stmt::Assign { name, value }
            | Stmt::Push {
                list: name, value, ..
            } => {
                walk.reference(&self.declared, name)?;
                self.expr(walk, value)?;
                Ok(statement.clone())
            }
            Stmt::Clear { list, .. } | Stmt::KeepBlobs { list, .. } => {
                walk.reference(&self.declared, list)?;
                Ok(statement.clone())
            }
            Stmt::Keep {
                list, count, first, ..
            } => {
                walk.reference(&self.declared, list)?;
                self.expr(walk, count)?;
                if let Some(first) = first {
                    self.expr(walk, first)?;
                }
                Ok(statement.clone())
            }
            Stmt::Return { value, .. } => {
                self.expr(walk, value)?;
                Ok(statement.clone())
            }
            Stmt::Rewrite { list, arms, .. } => {
                walk.reference(&self.declared, list)?;
                for arm in arms {
                    let bindings: &[Name] = match &arm.pattern {
                        Pattern::Variant { bindings, .. } => bindings,
                        Pattern::Value(_) => &[],
                    };
                    if let Some(entry) = &arm.entry {
                        walk.scoped(bindings, |walk| self.expr(walk, entry))?;
                    }
                }
                Ok(statement.clone())
            }
            Stmt::If { .. } | Stmt::Match { .. } | Stmt::Loop { .. } | Stmt::For { .. } => {
                self.compound(walk, statement)
            }
            Stmt::Call { callee, args }
                if callee.text == DENY || callee.text == ALLOW || callee.text == RESPOND =>
            {
                self.verdict(walk, callee, args)
            }
            Stmt::Call { callee, args } => self.call(walk, callee, args),
            Stmt::Inlined { .. } => panic!("the parser never writes an inlined block"),
            Stmt::Climb(climb) => self.climb(walk, climb),
            Stmt::Yield { kind, .. } => {
                match kind {
                    Yield::Candidate { value, hypothesis } => {
                        self.expr(walk, value)?;
                        self.expr(walk, hypothesis)?;
                    }
                    Yield::Score(values) => {
                        for value in values {
                            self.expr(walk, value)?;
                        }
                    }
                    Yield::Cost(value) => self.expr(walk, value)?,
                }
                Ok(statement.clone())
            }
        }
    }

    fn climb(&mut self, walk: &mut Walk<'a>, climb: &'a Climb) -> Checked<Stmt> {
        walk.reference(&self.declared, &climb.over)?;
        walk.reference(&self.declared, &climb.ledger)?;
        if let Some(end) = &climb.end {
            walk.reference(&self.declared, end)?;
        }
        if let Some(from) = &climb.from {
            self.expr(walk, from)?;
        }
        let metrics: Vec<Name> = climb
            .metrics
            .iter()
            .map(|metric| metric.name.clone())
            .collect();
        for gate in &climb.gates {
            walk.scoped(&metrics, |walk| self.expr(walk, gate))?;
        }
        if let Rule::When { names, condition } = &climb.rule {
            walk.scoped(names, |walk| self.expr(walk, condition))?;
        }
        let propose = walk.scoped(std::slice::from_ref(&climb.propose.param), |walk| {
            self.block(walk, &climb.propose.body)
        })?;
        let measure = walk.scoped(std::slice::from_ref(&climb.measure.param), |walk| {
            self.block(walk, &climb.measure.body)
        })?;
        let mut expanded = climb.clone();
        expanded.propose.body = propose;
        expanded.measure.body = measure;
        Ok(Stmt::Climb(Box::new(expanded)))
    }

    fn compound(&mut self, walk: &mut Walk<'a>, statement: &'a Stmt) -> Checked<Stmt> {
        match statement {
            Stmt::If {
                at,
                condition,
                then,
                otherwise,
            } => {
                self.expr(walk, condition)?;
                let then = self.block(walk, then)?;
                let otherwise = match otherwise {
                    Some(block) => Some(self.block(walk, block)?),
                    None => None,
                };
                Ok(Stmt::If {
                    at: *at,
                    condition: condition.clone(),
                    then,
                    otherwise,
                })
            }
            Stmt::Match { at, subject, arms } => {
                self.expr(walk, subject)?;
                let mut expanded = Vec::with_capacity(arms.len());
                for arm in arms {
                    let bindings: &[Name] = match &arm.pattern {
                        Pattern::Variant { bindings, .. } => bindings,
                        Pattern::Value(_) => &[],
                    };
                    let body = walk.scoped(bindings, |walk| self.block(walk, &arm.body))?;
                    expanded.push(Arm {
                        pattern: arm.pattern.clone(),
                        body,
                    });
                }
                Ok(Stmt::Match {
                    at: *at,
                    subject: subject.clone(),
                    arms: expanded,
                })
            }
            Stmt::Loop {
                at,
                name,
                bound,
                guard,
                body,
            } => {
                let body = walk.scoped(std::slice::from_ref(name), |walk| {
                    if let Some(guard) = guard {
                        self.expr(walk, guard)?;
                    }
                    self.block(walk, body)
                })?;
                Ok(Stmt::Loop {
                    at: *at,
                    name: name.clone(),
                    bound: bound.clone(),
                    guard: guard.clone(),
                    body,
                })
            }
            Stmt::For {
                at,
                name,
                list,
                parallel,
                body,
            } => {
                self.expr(walk, list)?;
                let body =
                    walk.scoped(std::slice::from_ref(name), |walk| self.block(walk, body))?;
                Ok(Stmt::For {
                    at: *at,
                    name: name.clone(),
                    list: list.clone(),
                    parallel: *parallel,
                    body,
                })
            }
            _ => panic!("compound walks only statements that hold blocks"),
        }
    }

    fn call(&mut self, walk: &mut Walk<'a>, callee: &'a Name, args: &'a [Expr]) -> Checked<Stmt> {
        let Some(&function) = self.functions.get(callee.text.as_str()) else {
            return error(
                callee.at,
                format!(
                    "'{}' is not a fn: only a fn is called as a statement; bind an effect's result with let or match it",
                    callee.text
                ),
            );
        };
        let name = function.name.text.as_str();
        self.called.insert(name);
        if args.len() != function.params.len() {
            return error(
                callee.at,
                format!(
                    "fn '{name}' takes {} argument(s), found {}",
                    function.params.len(),
                    args.len()
                ),
            );
        }
        for arg in args {
            self.expr(walk, arg)?;
        }
        self.function(function, callee.at)?;
        let expansion = &self.expanded[name];
        let depth = walk.depth + expansion.depth;
        if depth > NESTING_DEPTH_MAX {
            return error(
                callee.at,
                format!(
                    "the call of fn '{name}' nests blocks deeper than nesting_depth_max ({NESTING_DEPTH_MAX}) once inlined"
                ),
            );
        }
        walk.deepest = walk.deepest.max(depth);
        walk.count(callee.at, args.len() as u64 + expansion.statements)?;
        Ok(Stmt::Inlined {
            at: callee.at,
            function: function.name.clone(),
            arguments: function
                .params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect(),
            body: expansion.body.clone(),
            sees_caller: false,
        })
    }

    fn expr(&mut self, walk: &mut Walk<'a>, expr: &'a Expr) -> Checked<()> {
        let mut pending = vec![expr];
        while let Some(expr) = pending.pop() {
            match expr {
                Expr::Number(..) | Expr::Text(..) | Expr::Bool(..) => {}
                Expr::Name(name) => walk.reference(&self.declared, name)?,
                Expr::Member { base, .. } => pending.push(base),
                Expr::Call {
                    callee,
                    args,
                    settings,
                    ..
                } => {
                    if self.functions.contains_key(callee.text.as_str()) {
                        return error(
                            callee.at,
                            format!(
                                "fn '{}' has no value: call it as a statement of its own, not inside an expression",
                                callee.text
                            ),
                        );
                    }
                    walk.reference(&self.declared, callee)?;
                    pending.extend(args.iter().rev());
                    if let Some(settings) = settings {
                        pending.extend(settings.fields.iter().rev().map(|(_, value)| value));
                    }
                }
                Expr::Binary { left, right, .. }
                | Expr::With {
                    list: left,
                    entry: right,
                    ..
                } => {
                    pending.push(right);
                    pending.push(left);
                }
                Expr::Not { operand, .. } => pending.push(operand),
                Expr::Index { base, index, .. } => {
                    pending.push(index);
                    pending.push(base);
                }
                Expr::Take { .. } | Expr::Empty(_) => {}
            }
        }
        Ok(())
    }
}
