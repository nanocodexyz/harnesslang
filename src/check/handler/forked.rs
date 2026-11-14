use super::{Checker, statement_at};
use crate::check::{Checked, Globals, JOIN, error, resolve};
use crate::ir::{self, Expr, Stmt, Type};
use crate::syntax::ast::{self, HookKind};

const FORKED_RESULT: bool = true;

pub fn forked(program: &ir::Program, globals: &Globals, hook: &ast::Hook) -> Checked<ir::Handler> {
    assert_eq!(hook.kind, HookKind::Forked, "only on forked checks here");
    let mut checker = Checker {
        program,
        globals,
        locals: Vec::new(),
        scopes: vec![Vec::new()],
        sites: Vec::new(),
        returns: Type::Bool,
        reads_events: false,
        in_hook: true,
        spawn_subject: false,
        side: None,
        forked: true,
        used: std::collections::BTreeSet::new(),
    };
    let mut params = Vec::with_capacity(hook.params.len());
    for param in &hook.params {
        let ty = resolve(program, globals, &param.ty)?;
        params.push(checker.declare(&param.name, ty)?);
    }
    let (mut body, returned) = checker.block(&hook.body)?;
    assert!(!returned, "the checker refuses a return in on forked");
    assert!(
        checker.sites.is_empty(),
        "the checker refuses every effect in on forked"
    );
    body.push(Stmt::Finish {
        value: Expr::Bool(FORKED_RESULT),
    });
    let mut handler = ir::Handler {
        name: ir::FORKED_HOOK.to_owned(),
        params,
        returns: Type::Bool,
        requires: None,
        locals: std::mem::take(&mut checker.locals),
        sites: Vec::new(),
        body,
        worst: ir::worst::WorstCase::default(),
    };
    handler.worst = ir::worst::compute(program, &handler).map_err(|overflow| {
        crate::syntax::Diagnostic::new(
            hook.at,
            format!("'on forked': worst-case {} overflow u64", overflow.measure),
        )
    })?;
    Ok(handler)
}

impl Checker<'_> {
    pub(super) fn forked_statement(&self, statement: &ast::Stmt) -> Checked<()> {
        if !self.forked {
            return Ok(());
        }
        let what = match statement {
            ast::Stmt::Return { .. } => "return",
            ast::Stmt::For { parallel: true, .. } => "dispatch a parallel for",
            _ => return Ok(()),
        };
        error(
            statement_at(statement),
            format!(
                "'on forked' cannot {what}: it only shapes the state a fork starts from, and the fork's turn goes on"
            ),
        )
    }

    pub(super) fn forked_parallel(&self, at: crate::syntax::Position) -> Checked<()> {
        let Some(fork) = self.program.tools.iter().find(|tool| tool.fork.is_some()) else {
            return Ok(());
        };
        error(
            at,
            format!(
                "a parallel for dispatches tool calls at once, but fork tool '{}' fills its input from the journal before its call record is written: use a sequential for",
                fork.name
            ),
        )
    }

    pub(super) fn forked_call(&self, callee: &ast::Name) -> Checked<()> {
        if !self.forked {
            return Ok(());
        }
        let name = callee.text.as_str();
        let what = if self.globals.tools.contains_key(name) {
            format!("call tool '{name}'")
        } else if self.globals.model.as_deref() == Some(name) {
            format!("call the model '{name}'")
        } else if self.globals.codes.contains_key(name) {
            format!("run code tool '{name}'")
        } else if self.globals.listings.contains_key(name) {
            format!("read listing '{name}'")
        } else if ["dispatch", "wait", "hold", "suspend", "now", "random", JOIN].contains(&name) {
            format!("call '{name}'")
        } else {
            return Ok(());
        };
        error(
            callee.at,
            format!(
                "'on forked' cannot {what}: it runs inside the fork's call, before the call is journaled, so it runs no effect"
            ),
        )
    }
}
