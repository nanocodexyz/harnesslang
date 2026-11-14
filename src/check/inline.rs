use std::collections::{BTreeMap, BTreeSet};

use crate::check::{BUILTIN_FUNCTIONS, Checked, Globals, WILDCARD, claim, error, variant_names};
use crate::ir;
use crate::syntax::Position;
use crate::syntax::ast::{self, Arm, Block, Expr, Function, Item, Name, Pattern, Stmt};
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
}

impl Owner<'_> {
    fn describe(self) -> String {
        match self {
            Self::Handler(name) => format!("handler '{name}'"),
            Self::Function(name) => format!("fn '{name}'"),
            Self::Invariant(name) => format!("invariant '{name}'"),
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

    fn bind(&mut self, name: &Name) {
        if name.text == WILDCARD {
            return;
        }
        self.scopes
            .last_mut()
            .expect("a walk always has a scope")
            .push(name.text.clone());
    }

    fn scoped<T>(
        &mut self,
        names: &[Name],
        body: impl FnOnce(&mut Self) -> Checked<T>,
    ) -> Checked<T> {
        self.scopes.push(Vec::new());
        names.iter().for_each(|name| self.bind(name));
        let result = body(self);
        self.scopes.pop().expect("every pushed scope is popped");
        result
    }

    fn reference(&self, declared: &BTreeSet<String>, name: &Name) -> Checked<()> {
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
        declared: declared(globals, &variants),
        expanded: BTreeMap::new(),
        called: BTreeSet::new(),
        stack: Vec::new(),
    };
    let functions: Vec<&Function> = source
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => Some(function),
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

fn declared(globals: &Globals, variants: &BTreeSet<String>) -> BTreeSet<String> {
    let mut names = variants.clone();
    names.extend(globals.fields.keys().cloned());
    names.extend(globals.constants.keys().cloned());
    names.extend(globals.tools.keys().cloned());
    names.extend(globals.listings.keys().cloned());
    names.extend(globals.model.iter().cloned());
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
            .for_each(|param| walk.bind(&param.name));
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
            .for_each(|param| walk.bind(&param.name));
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
            Stmt::Let { name, value, .. } => {
                self.expr(walk, value)?;
                walk.bind(name);
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
            Stmt::Clear { list, .. } => {
                walk.reference(&self.declared, list)?;
                Ok(statement.clone())
            }
            Stmt::Keep { list, count, .. } => {
                walk.reference(&self.declared, list)?;
                self.expr(walk, count)?;
                Ok(statement.clone())
            }
            Stmt::Return { value, .. } => {
                self.expr(walk, value)?;
                Ok(statement.clone())
            }
            Stmt::If { .. } | Stmt::Match { .. } | Stmt::Loop { .. } | Stmt::For { .. } => {
                self.compound(walk, statement)
            }
            Stmt::Call { callee, args } => self.call(walk, callee, args),
            Stmt::Inlined { .. } => panic!("the parser never writes an inlined block"),
        }
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
                body,
            } => {
                let body =
                    walk.scoped(std::slice::from_ref(name), |walk| self.block(walk, body))?;
                Ok(Stmt::Loop {
                    at: *at,
                    name: name.clone(),
                    bound: bound.clone(),
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
        })
    }

    fn expr(&mut self, walk: &mut Walk<'a>, expr: &'a Expr) -> Checked<()> {
        let mut pending = vec![expr];
        while let Some(expr) = pending.pop() {
            match expr {
                Expr::Number(..) | Expr::Text(..) | Expr::Bool(..) => {}
                Expr::Name(name) => walk.reference(&self.declared, name)?,
                Expr::Member { base, .. } => pending.push(base),
                Expr::Call { callee, args } => {
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
                }
                Expr::Binary { left, right, .. } => {
                    pending.push(right);
                    pending.push(left);
                }
                Expr::Not { operand, .. } => pending.push(operand),
            }
        }
        Ok(())
    }
}
