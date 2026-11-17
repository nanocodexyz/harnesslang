use std::collections::BTreeSet;

use super::{Checker, statement_at};
use crate::check::{Checked, Globals, bound, error, resolve};
use crate::ir::{self, BytesOp, Expr, LocalId, Stmt, Type};
use crate::syntax::Position;
use crate::syntax::ast;

const IMPURE_BUILTINS: [&str; 7] = [
    "dispatch",
    "now",
    "random",
    "wait",
    "hold",
    "suspend",
    ir::SPENT_BUILTIN,
];

pub fn function(
    program: &ir::Program,
    globals: &Globals,
    definition: &ast::Function,
) -> Checked<ir::Handler> {
    let name = definition.name.text.clone();
    let returns = definition
        .returns
        .as_ref()
        .expect("only a fn with a result is checked as a pure fn");
    let returns = resolve_local(program, globals, returns)?;
    let mut set_names = BTreeSet::new();
    set_targets(&definition.body, &mut set_names);
    let mut checker = Checker::new(program, globals, returns.clone());
    checker.pure = Some(name.clone());
    checker.set_names = set_names;
    let mut params = Vec::with_capacity(definition.params.len());
    for param in &definition.params {
        let ty = resolve_local(program, globals, &param.ty)?;
        params.push(checker.declare(&param.name, ty)?);
    }
    let (body, returned) = checker.block(&definition.body)?;
    if !returned {
        return error(
            definition.body.at,
            format!("fn '{name}' can reach its end without returning"),
        );
    }
    assert!(checker.sites.is_empty(), "a pure fn holds no effect site");
    Ok(ir::Handler {
        name,
        params,
        returns,
        requires: None,
        locals: std::mem::take(&mut checker.locals),
        sites: Vec::new(),
        body,
        worst: ir::worst::WorstCase::default(),
    })
}

pub fn resolve_local(
    program: &ir::Program,
    globals: &Globals,
    ty: &ast::TypeExpr,
) -> Checked<Type> {
    match ty {
        ast::TypeExpr::Bytes { capacity, .. } => Ok(Type::Bytes(bound(
            program,
            globals,
            capacity,
            "the bytes a byte buffer may hold",
        )?)),
        _ => resolve(program, globals, ty),
    }
}

fn set_targets(block: &ast::Block, out: &mut BTreeSet<String>) {
    for statement in &block.statements {
        match statement {
            ast::Stmt::SetIndex { list, .. } => {
                out.insert(list.text.clone());
            }
            ast::Stmt::If {
                then, otherwise, ..
            } => {
                set_targets(then, out);
                if let Some(otherwise) = otherwise {
                    set_targets(otherwise, out);
                }
            }
            ast::Stmt::Match { arms, .. } => {
                arms.iter().for_each(|arm| set_targets(&arm.body, out));
            }
            ast::Stmt::Loop { body, .. } | ast::Stmt::For { body, .. } => set_targets(body, out),
            _ => {}
        }
    }
}

fn fresh(value: &Expr) -> bool {
    matches!(
        value,
        Expr::Copy { .. }
            | Expr::Bytes {
                op: BytesOp::Empty
                    | BytesOp::EmptyList
                    | BytesOp::OfText
                    | BytesOp::OfHex
                    | BytesOp::Slice,
                ..
            }
    )
}

impl Checker<'_> {
    pub(super) fn pure_only(&self, at: Position, what: &str) -> Checked<()> {
        if self.pure.is_none() {
            return error(
                at,
                format!(
                    "{what} lives only in a pure fn (fn f(...) -> T), which computes without effects"
                ),
            );
        }
        Ok(())
    }

    pub(super) fn pure_statement(&self, statement: &ast::Stmt) -> Checked<()> {
        let Some(function) = &self.pure else {
            return Ok(());
        };
        let what = match statement {
            ast::Stmt::Clear { .. }
            | ast::Stmt::Keep { .. }
            | ast::Stmt::KeepBlobs { .. }
            | ast::Stmt::Rewrite { .. } => "changes no state field",
            ast::Stmt::For { parallel: true, .. } => "dispatches no parallel for",
            ast::Stmt::Climb(_) | ast::Stmt::Yield { .. } => "holds no climb",
            ast::Stmt::Call { .. } | ast::Stmt::Inlined { .. } => {
                "calls only pure fns, inside expressions"
            }
            _ => return Ok(()),
        };
        error(
            statement_at(statement),
            format!("fn '{function}' is pure: it {what}"),
        )
    }

    pub(super) fn var_statement(
        &mut self,
        name: &ast::Name,
        ty: Option<&ast::TypeExpr>,
        value: &ast::Expr,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        self.pure_only(name.at, "a var")?;
        let wanted = match ty {
            Some(ty) => Some(resolve_local(self.program, self.globals, ty)?),
            None => None,
        };
        let at = value.at();
        let (value, ty) = self.expr(value, wanted.as_ref(), out)?;
        if let Some(wanted) = &wanted {
            self.expect(at, wanted, &ty)?;
        }
        let local = self.declare(name, ty)?;
        self.vars.insert(local);
        if self.set_names.contains(&name.text) {
            self.owned.insert(local);
        }
        let value = self.owned_value(local, value);
        out.push(Stmt::Let { local, value });
        Ok(false)
    }

    fn owned_value(&self, local: LocalId, value: Expr) -> Expr {
        if !self.owned.contains(&local) || fresh(&value) {
            return value;
        }
        let bytes = matches!(self.locals[local].ty, Type::Bytes(_));
        match &self.locals[local].ty {
            Type::List { .. } | Type::Bytes(_) => Expr::Copy {
                value: Box::new(value),
                bytes,
            },
            _ => value,
        }
    }

    fn var(&self, name: &ast::Name, action: &str) -> Checked<Option<LocalId>> {
        let Some(local) = self.lookup(&name.text) else {
            if let Some(function) = &self.pure {
                return error(
                    name.at,
                    format!(
                        "fn '{function}' is pure: it cannot {action} '{}', since it changes no state field",
                        name.text
                    ),
                );
            }
            return Ok(None);
        };
        if !self.vars.contains(&local) {
            if self.pure.is_none() {
                return Ok(None);
            }
            return error(
                name.at,
                format!(
                    "cannot {action} '{}': it is a let or a parameter, and only a var changes",
                    name.text
                ),
            );
        }
        Ok(Some(local))
    }

    pub(super) fn assign_local(
        &mut self,
        name: &ast::Name,
        value: &ast::Expr,
        out: &mut Vec<Stmt>,
    ) -> Checked<Option<bool>> {
        let Some(local) = self.var(name, "assign to")? else {
            return Ok(None);
        };
        let wanted = self.locals[local].ty.clone();
        let at = value.at();
        let (value, ty) = self.expr(value, Some(&wanted), out)?;
        self.expect(at, &wanted, &ty)?;
        let value = self.owned_value(local, value);
        out.push(Stmt::Set { local, value });
        Ok(Some(false))
    }

    pub(super) fn push_local(
        &mut self,
        list: &ast::Name,
        value: &ast::Expr,
        out: &mut Vec<Stmt>,
    ) -> Checked<Option<bool>> {
        let Some(local) = self.var(list, "push onto")? else {
            return Ok(None);
        };
        let at = value.at();
        match self.locals[local].ty.clone() {
            Type::List { element, .. } => {
                let (value, ty) = self.expr(value, Some(&element), out)?;
                self.expect(at, &element, &ty)?;
                out.push(Stmt::PushLocal {
                    local,
                    value,
                    append: false,
                });
            }
            Type::Bytes(_) => {
                let (value, ty) = self.expr(value, Some(&Type::U64), out)?;
                let append = match ty {
                    Type::U64 => false,
                    Type::Bytes(_) => true,
                    other => {
                        return error(
                            at,
                            format!(
                                "push onto bytes takes a byte (u64) or bytes, found {}",
                                self.type_name(&other)
                            ),
                        );
                    }
                };
                out.push(Stmt::PushLocal {
                    local,
                    value,
                    append,
                });
            }
            other => {
                return error(
                    list.at,
                    format!(
                        "cannot push onto '{}': it is {}, not a list or bytes",
                        list.text,
                        self.type_name(&other)
                    ),
                );
            }
        }
        Ok(Some(false))
    }

    pub(super) fn set_index(
        &mut self,
        at: Position,
        list: &ast::Name,
        index: &ast::Expr,
        value: &ast::Expr,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        self.pure_only(at, "set xs[i] = v")?;
        let local = self
            .var(list, "set an entry of")?
            .expect("a pure fn refuses names that are not locals");
        let element = match &self.locals[local].ty {
            Type::List { element, .. } => (**element).clone(),
            Type::Bytes(_) => Type::U64,
            other => {
                return error(
                    list.at,
                    format!(
                        "cannot set an entry of '{}': it is {}, not a list or bytes",
                        list.text,
                        self.type_name(other)
                    ),
                );
            }
        };
        let index_at = index.at();
        let (index, index_type) = self.expr(index, Some(&Type::U64), out)?;
        self.expect(index_at, &Type::U64, &index_type)?;
        let value_at = value.at();
        let (value, ty) = self.expr(value, Some(&element), out)?;
        self.expect(value_at, &element, &ty)?;
        out.push(Stmt::SetIndex {
            local,
            index,
            value,
        });
        Ok(false)
    }

    pub(super) fn while_statement(
        &mut self,
        name: &ast::Name,
        limit: &ast::Name,
        guard: &ast::Expr,
        body: &ast::Block,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        self.pure_only(guard.at(), "a loop with while")?;
        let limit = bound(
            self.program,
            self.globals,
            limit,
            &format!("loop '{}'", name.text),
        )?;
        let (index, condition, body) = self.scoped(|checker| {
            let index = checker.declare(name, Type::U64)?;
            let condition =
                checker.condition(guard, &format!("the while of loop '{}'", name.text))?;
            Ok((index, condition, checker.block(body)?.0))
        })?;
        out.push(Stmt::While {
            index,
            bound: limit,
            condition,
            body,
        });
        Ok(false)
    }

    pub(super) fn borrowed(
        &mut self,
        value: &ast::Expr,
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        if let ast::Expr::Name(name) = value
            && let Some(local) = self.lookup(&name.text)
        {
            return Ok((Expr::Local(local), self.locals[local].ty.clone()));
        }
        self.expr(value, None, out)
    }

    pub(super) fn local_read(&self, local: LocalId) -> (Expr, Type) {
        let ty = self.locals[local].ty.clone();
        if self.owned.contains(&local) {
            let bytes = matches!(ty, Type::Bytes(_));
            return (
                Expr::Copy {
                    value: Box::new(Expr::Local(local)),
                    bytes,
                },
                ty,
            );
        }
        (Expr::Local(local), ty)
    }

    pub(super) fn index(
        &mut self,
        at: Position,
        base: &ast::Expr,
        index: &ast::Expr,
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        self.pure_only(at, "an indexed read xs[i]")?;
        let (base, base_type) = self.borrowed(base, out)?;
        let element = match &base_type {
            Type::List { element, .. } => (**element).clone(),
            Type::Bytes(_) => Type::U64,
            other => {
                return error(
                    at,
                    format!(
                        "only a list or bytes is indexed, not {}",
                        self.type_name(other)
                    ),
                );
            }
        };
        let index_at = index.at();
        let (index, index_type) = self.expr(index, Some(&Type::U64), out)?;
        self.expect(index_at, &Type::U64, &index_type)?;
        if matches!(base_type, Type::Bytes(_)) {
            return Ok((
                Expr::Bytes {
                    op: BytesOp::Get,
                    args: vec![base, index],
                },
                element,
            ));
        }
        Ok((
            Expr::Index {
                base: Box::new(base),
                index: Box::new(index),
            },
            element,
        ))
    }

    pub(super) fn empty(&self, at: Position, expected: Option<&Type>) -> Checked<(Expr, Type)> {
        self.pure_only(at, "[]")?;
        let op = match expected {
            Some(Type::List { .. }) => BytesOp::EmptyList,
            Some(Type::Bytes(_)) => BytesOp::Empty,
            _ => {
                return error(
                    at,
                    "[] is an empty list or bytes, and takes its type from where it goes, such as var xs: list<u64, N> = []",
                );
            }
        };
        Ok((
            Expr::Bytes {
                op,
                args: Vec::new(),
            },
            expected.expect("an empty value has a type").clone(),
        ))
    }

    pub(super) fn pure_call(
        &mut self,
        callee: &ast::Name,
        function: ir::FunctionId,
        args: &[ast::Expr],
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        let Some(definition) = self.program.functions.get(function) else {
            let caller = self.pure.as_deref().unwrap_or("this fn");
            let callee_text = if callee.text == caller {
                "itself".to_owned()
            } else {
                format!("fn '{}', declared below it", callee.text)
            };
            return error(
                callee.at,
                format!(
                    "fn '{caller}' calls {callee_text}: a pure fn calls only the pure fns above it, so it never recurses"
                ),
            );
        };
        if args.len() != definition.params.len() {
            return error(
                callee.at,
                format!(
                    "fn '{}' takes {} arguments, found {}",
                    callee.text,
                    definition.params.len(),
                    args.len()
                ),
            );
        }
        if self.pure.is_none() && definition.returns.holds_bytes(self.program) {
            return error(
                callee.at,
                format!(
                    "fn '{}' returns bytes, which live only inside pure fns",
                    callee.text
                ),
            );
        }
        let wanted: Vec<Type> = definition
            .params
            .iter()
            .map(|param| definition.locals[*param].ty.clone())
            .collect();
        let returns = definition.returns.clone();
        let mut values = Vec::with_capacity(args.len());
        for (arg, wanted) in args.iter().zip(&wanted) {
            let at = arg.at();
            let (value, ty) = self.expr(arg, Some(wanted), out)?;
            self.expect(at, wanted, &ty)?;
            values.push(value);
        }
        Ok((
            Expr::Call {
                function,
                args: values,
            },
            returns,
        ))
    }

    pub(super) fn refuse_impure(&self, callee: &ast::Name) -> Checked<()> {
        let Some(function) = &self.pure else {
            return Ok(());
        };
        let name = callee.text.as_str();
        let effect = self.globals.effect(name)
            || self.globals.model.as_deref() == Some(name)
            || IMPURE_BUILTINS.contains(&name);
        if effect {
            return error(
                callee.at,
                format!(
                    "fn '{function}' is pure: it cannot call '{name}', since it calls no tool, model or effect and reads no clock"
                ),
            );
        }
        Ok(())
    }

    pub(super) fn bytes_builtin(
        &mut self,
        at: Position,
        op: BytesOp,
        args: &[ast::Expr],
        expected: Option<&Type>,
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        self.pure_only(at, &format!("{}(...)", op.name()))?;
        let arity = if matches!(op, BytesOp::OfText | BytesOp::OfHex) {
            1
        } else {
            op.arity()
        };
        if args.len() != arity {
            return error(
                at,
                format!(
                    "{} takes {arity} arguments, found {}",
                    op.name(),
                    args.len()
                ),
            );
        }
        let mut values = Vec::with_capacity(arity);
        let result = match op {
            BytesOp::OfText | BytesOp::OfHex => {
                let Some(ty @ Type::Bytes(_)) = expected else {
                    return error(
                        at,
                        format!(
                            "{name} takes its capacity from where it goes, such as let b: bytes<N> = {name}(t)",
                            name = op.name()
                        ),
                    );
                };
                let ty = ty.clone();
                let Type::Bytes(capacity) = &ty else {
                    unreachable!("the pattern above matched bytes");
                };
                let constant = self
                    .program
                    .constants
                    .iter()
                    .position(|constant| constant.name == capacity.name)
                    .expect("a capacity names a constant");
                let arg_at = args[0].at();
                let (value, found) = self.expr(&args[0], Some(&Type::Text), out)?;
                self.expect(arg_at, &Type::Text, &found)?;
                values.push(value);
                values.push(Expr::Constant(constant));
                ty
            }
            BytesOp::ToText | BytesOp::ToHex | BytesOp::Slice => {
                let arg_at = args[0].at();
                let (value, found) = self.borrowed(&args[0], out)?;
                if !matches!(found, Type::Bytes(_)) {
                    return error(
                        arg_at,
                        format!(
                            "{} takes bytes, found {}",
                            op.name(),
                            self.type_name(&found)
                        ),
                    );
                }
                values.push(value);
                for arg in &args[1..] {
                    let arg_at = arg.at();
                    let (value, ty) = self.expr(arg, Some(&Type::U64), out)?;
                    self.expect(arg_at, &Type::U64, &ty)?;
                    values.push(value);
                }
                if matches!(op, BytesOp::ToText | BytesOp::ToHex) {
                    Type::Text
                } else {
                    found
                }
            }
            BytesOp::Get | BytesOp::Empty | BytesOp::EmptyList => {
                unreachable!(
                    "only bytes_of, text_of, bytes_of_hex, hex_of and slice are called by name"
                )
            }
        };
        Ok((Expr::Bytes { op, args: values }, result))
    }
}
