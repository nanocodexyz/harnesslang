use crate::check::{
    CANCELLED_READ, Checked, EVENT_STEER, Globals, STEER_PARAMETER, STEERS_READ, WILDCARD, bound,
    error, quoted, resolve,
};
use crate::effect::EffectClass;
use crate::ir::{self, BinaryOp, Durability, Expr, LocalId, Site, SiteClass, SiteKind, Stmt, Type};
use crate::syntax::Position;
use crate::syntax::ast;

struct Checker<'a> {
    program: &'a ir::Program,
    globals: &'a Globals,
    locals: Vec<ir::Field>,
    scopes: Vec<Vec<(String, LocalId)>>,
    sites: Vec<Site>,
    returns: Type,
    reads_events: bool,
}

pub fn check(
    program: &ir::Program,
    globals: &Globals,
    definition: &ast::Handler,
) -> Checked<ir::Handler> {
    let returns = resolve(program, globals, &definition.returns)?;
    let mut checker = Checker {
        program,
        globals,
        locals: Vec::new(),
        scopes: vec![Vec::new()],
        sites: Vec::new(),
        returns: returns.clone(),
        reads_events: false,
    };
    let mut params = Vec::new();
    for param in &definition.params {
        let ty = resolve(program, globals, &param.ty)?;
        params.push(checker.declare(&param.name, ty)?);
    }
    let requires = match &definition.requires {
        Some(condition) => Some(checker.condition(
            condition,
            &format!("the requires of handler '{}'", definition.name.text),
        )?),
        None => None,
    };
    checker.reads_events = true;
    let (body, returned) = checker.block(&definition.body)?;
    if !returned {
        return error(
            definition.body.at,
            format!(
                "handler '{}' can reach its end without returning",
                definition.name.text
            ),
        );
    }
    let mut handler = ir::Handler {
        name: definition.name.text.clone(),
        params,
        returns,
        requires,
        locals: checker.locals,
        sites: checker.sites,
        body,
        worst: ir::worst::WorstCase::default(),
    };
    handler.worst = ir::worst::compute(&handler).map_err(|overflow| {
        crate::syntax::Diagnostic::new(
            definition.name.at,
            format!(
                "handler '{}': worst-case {} overflow u64 (loops: {})",
                definition.name.text,
                overflow.measure,
                ir::worst::Cost {
                    total: 0,
                    loops: overflow.loops
                }
                .describe_loops()
            ),
        )
    })?;
    Ok(handler)
}

pub fn invariant(
    program: &ir::Program,
    globals: &Globals,
    definition: &ast::Invariant,
) -> Checked<ir::Invariant> {
    let mut checker = Checker {
        program,
        globals,
        locals: Vec::new(),
        scopes: vec![Vec::new()],
        sites: Vec::new(),
        returns: Type::Bool,
        reads_events: false,
    };
    let condition = checker.condition(
        &definition.condition,
        &format!("invariant '{}'", definition.name.text),
    )?;
    Ok(ir::Invariant {
        name: definition.name.text.clone(),
        condition,
    })
}

impl Checker<'_> {
    fn condition(&mut self, condition: &ast::Expr, what: &str) -> Checked<Expr> {
        let at = condition.at();
        let mut out = Vec::new();
        let (value, ty) = self.expr(condition, Some(&Type::Bool), &mut out)?;
        self.expect(at, &Type::Bool, &ty)?;
        if !out.is_empty() {
            return error(at, format!("{what} must be pure: it calls an effect"));
        }
        Ok(value)
    }

    fn type_name(&self, ty: &Type) -> String {
        ty.name(self.program)
    }

    fn lookup(&self, name: &str) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|(bound, _)| bound == name)
            .map(|(_, local)| *local)
    }

    fn declare(&mut self, name: &ast::Name, ty: Type) -> Checked<LocalId> {
        if name.text.starts_with(WILDCARD) {
            return error(
                name.at,
                format!("'{}': names starting with '_' are reserved", name.text),
            );
        }
        if self.lookup(&name.text).is_some() {
            return error(
                name.at,
                format!("'{}' is already bound in this handler", name.text),
            );
        }
        if self.program.events.cancel && name.text == CANCELLED_READ {
            return error(
                name.at,
                format!("'{CANCELLED_READ}' is the cancel event's read: choose another name"),
            );
        }
        if self.globals.fields.contains_key(&name.text)
            || self.globals.constants.contains_key(&name.text)
        {
            return error(
                name.at,
                format!("'{}' is already a state field or constant", name.text),
            );
        }
        if self.globals.effect(&name.text) {
            return error(
                name.at,
                format!("'{}' is already a tool, listing or model", name.text),
            );
        }
        Ok(self.bind(name.text.clone(), ty))
    }

    fn bind(&mut self, name: String, ty: Type) -> LocalId {
        let local = self.locals.len();
        self.locals.push(ir::Field {
            name: name.clone(),
            ty,
        });
        self.scopes
            .last_mut()
            .expect("a handler always has a scope")
            .push((name, local));
        local
    }

    fn take(
        &mut self,
        at: Position,
        events: &ast::Name,
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        if events.text != STEERS_READ {
            return error(
                events.at,
                format!("'take' reads '{STEERS_READ}', found '{}'", events.text),
            );
        }
        let Some(ty) = self.program.events.steers_type() else {
            return error(
                at,
                format!(
                    "'take {STEERS_READ}' needs 'on {EVENT_STEER}({STEER_PARAMETER}: text) bound N' on the harness"
                ),
            );
        };
        self.events_readable(at, &format!("take {STEERS_READ}"))?;
        let local = self.locals.len();
        self.locals.push(ir::Field {
            name: format!("{WILDCARD}take{local}"),
            ty: ty.clone(),
        });
        out.push(Stmt::Take { result: local });
        Ok((Expr::Local(local), ty))
    }

    fn events_readable(&self, at: Position, what: &str) -> Checked<()> {
        if self.reads_events {
            return Ok(());
        }
        error(
            at,
            format!("'{what}' reads the turn's events, so it belongs in a handler's body"),
        )
    }

    fn temporary(&mut self, site: usize, ty: Type) -> LocalId {
        let local = self.locals.len();
        self.locals.push(ir::Field {
            name: format!("{WILDCARD}site{site}"),
            ty,
        });
        local
    }

    fn scoped<T>(&mut self, body: impl FnOnce(&mut Self) -> Checked<T>) -> Checked<T> {
        self.scopes.push(Vec::new());
        let result = body(self);
        self.scopes.pop().expect("every pushed scope is popped");
        result
    }

    fn block(&mut self, block: &ast::Block) -> Checked<(Vec<Stmt>, bool)> {
        self.scoped(|checker| {
            let mut out = Vec::new();
            let mut returned = false;
            for statement in &block.statements {
                if returned {
                    return error(
                        statement_at(statement),
                        "statement is unreachable after a return",
                    );
                }
                returned = checker.statement(statement, &mut out)?;
            }
            Ok((out, returned))
        })
    }

    fn expect(&self, at: Position, wanted: &Type, found: &Type) -> Checked<()> {
        if wanted == found {
            return Ok(());
        }
        error(
            at,
            format!(
                "expected {}, found {}",
                self.type_name(wanted),
                self.type_name(found)
            ),
        )
    }

    fn state_field(&self, name: &ast::Name, action: &str) -> Checked<ir::FieldId> {
        if let Some(field) = self.globals.fields.get(&name.text) {
            return Ok(*field);
        }
        if self.lookup(&name.text).is_some() {
            return error(
                name.at,
                format!(
                    "cannot {action} '{}': it is a local binding, and only state fields change",
                    name.text
                ),
            );
        }
        error(
            name.at,
            format!("cannot {action} '{}': unknown state field", name.text),
        )
    }

    fn statement(&mut self, statement: &ast::Stmt, out: &mut Vec<Stmt>) -> Checked<bool> {
        match statement {
            ast::Stmt::Let { name, ty, value } => {
                let wanted = match ty {
                    Some(ty) => Some(resolve(self.program, self.globals, ty)?),
                    None => None,
                };
                let at = value.at();
                let (value, ty) = self.expr(value, wanted.as_ref(), out)?;
                if let Some(wanted) = &wanted {
                    self.expect(at, wanted, &ty)?;
                }
                let local = self.declare(name, ty)?;
                out.push(Stmt::Let { local, value });
                Ok(false)
            }
            ast::Stmt::Assign { name, value } => {
                let field = self.state_field(name, "assign to")?;
                let wanted = self.program.state[field].ty.clone();
                let at = value.at();
                let (value, ty) = self.expr(value, Some(&wanted), out)?;
                self.expect(at, &wanted, &ty)?;
                out.push(Stmt::Assign { field, value });
                Ok(false)
            }
            ast::Stmt::Push { list, value, .. } => {
                let field = self.state_field(list, "push onto")?;
                let Type::List { element, .. } = self.program.state[field].ty.clone() else {
                    return error(
                        list.at,
                        format!("cannot push onto '{}': it is not a list", list.text),
                    );
                };
                let at = value.at();
                let (value, ty) = self.expr(value, Some(&element), out)?;
                self.expect(at, &element, &ty)?;
                out.push(Stmt::Push { field, value });
                Ok(false)
            }
            ast::Stmt::If {
                condition,
                then,
                otherwise,
                ..
            } => self.if_statement(condition, then, otherwise.as_ref(), out),
            ast::Stmt::Match { at, subject, arms } => self.match_statement(*at, subject, arms, out),
            ast::Stmt::Loop {
                name,
                bound: limit,
                body,
                ..
            } => self.loop_statement(name, limit, body, out),
            ast::Stmt::For {
                at,
                name,
                list,
                parallel,
                body,
            } => {
                if *parallel {
                    self.parallel_statement(*at, name, list, body, out)
                } else {
                    self.for_statement(name, list, body, out)
                }
            }
            ast::Stmt::Return { value, .. } => {
                let wanted = self.returns.clone();
                let at = value.at();
                let (value, ty) = self.expr(value, Some(&wanted), out)?;
                self.expect(at, &wanted, &ty)?;
                out.push(Stmt::Finish { value });
                Ok(true)
            }
            ast::Stmt::Inlined {
                arguments, body, ..
            } => self.inlined(arguments, body, out),
            ast::Stmt::Call { callee, .. } => {
                panic!(
                    "fn calls are inlined before checking, yet '{}' is still a call",
                    callee.text
                )
            }
        }
    }

    fn inlined(
        &mut self,
        arguments: &[(ast::Param, ast::Expr)],
        body: &ast::Block,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let mut parameters = Vec::with_capacity(arguments.len());
        for (param, argument) in arguments {
            let wanted = resolve(self.program, self.globals, &param.ty)?;
            let at = argument.at();
            let (value, ty) = self.expr(argument, Some(&wanted), out)?;
            self.expect(at, &wanted, &ty)?;
            let caller = std::mem::replace(&mut self.scopes, vec![parameters]);
            let declared = self.declare(&param.name, ty);
            let mut own = std::mem::replace(&mut self.scopes, caller);
            parameters = own.pop().expect("the fn's parameters are its one scope");
            assert!(own.is_empty(), "declaring a parameter adds no scope");
            out.push(Stmt::Let {
                local: declared?,
                value,
            });
        }
        let caller = std::mem::replace(&mut self.scopes, vec![parameters]);
        let checked = self.block(body);
        let own = std::mem::replace(&mut self.scopes, caller);
        assert_eq!(own.len(), 1, "a fn body leaves only its parameters' scope");
        let (statements, returned) = checked?;
        out.extend(statements);
        Ok(returned)
    }

    fn if_statement(
        &mut self,
        condition: &ast::Expr,
        then: &ast::Block,
        otherwise: Option<&ast::Block>,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let at = condition.at();
        let (condition, ty) = self.expr(condition, Some(&Type::Bool), out)?;
        self.expect(at, &Type::Bool, &ty)?;
        let (then, then_returns) = self.block(then)?;
        let (otherwise, otherwise_returns) = match otherwise {
            Some(block) => self.block(block)?,
            None => (Vec::new(), false),
        };
        out.push(Stmt::If {
            condition,
            then,
            otherwise,
        });
        Ok(then_returns && otherwise_returns)
    }

    fn loop_statement(
        &mut self,
        name: &ast::Name,
        limit: &ast::Name,
        body: &ast::Block,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let limit = bound(
            self.program,
            self.globals,
            limit,
            &format!("loop '{}'", name.text),
        )?;
        let (index, body) = self.scoped(|checker| {
            let index = checker.declare(name, Type::U64)?;
            Ok((index, checker.block(body)?.0))
        })?;
        out.push(Stmt::Loop {
            index,
            bound: limit,
            body,
        });
        Ok(false)
    }

    fn for_statement(
        &mut self,
        name: &ast::Name,
        list: &ast::Expr,
        body: &ast::Block,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let at = list.at();
        let (list, ty) = self.expr(list, None, out)?;
        let Type::List { element, capacity } = ty else {
            return error(
                at,
                format!("'for' walks a list, not {}", self.type_name(&ty)),
            );
        };
        let (item, body) = self.scoped(|checker| {
            let item = checker.declare(name, *element)?;
            Ok((item, checker.block(body)?.0))
        })?;
        out.push(Stmt::For {
            item,
            list,
            bound: capacity,
            body,
        });
        Ok(false)
    }

    fn parallel_statement(
        &mut self,
        at: Position,
        name: &ast::Name,
        list: &ast::Expr,
        body: &ast::Block,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let list_at = list.at();
        let (list, ty) = self.expr(list, None, out)?;
        let Type::List { element, capacity } = ty else {
            return error(
                list_at,
                format!("'for' walks a list, not {}", self.type_name(&ty)),
            );
        };
        let (match_at, subject, arms) = parallel_body(at, name, body)?;
        for arm in arms {
            for statement in &arm.body.statements {
                self.parallel_push(statement)?;
            }
        }
        let (item, lowered) = self.scoped(|checker| {
            let item = checker.declare(name, *element)?;
            let mut lowered = Vec::new();
            let returned = checker.match_statement(match_at, subject, arms, &mut lowered)?;
            assert!(!returned, "the arms of a parallel for only push");
            Ok((item, lowered))
        })?;
        let [Stmt::Effect { site, args, result }, tail @ ..] = lowered.as_slice() else {
            panic!("a parallel for's match starts with its dispatch, found {lowered:?}");
        };
        assert_eq!(
            self.sites[*site].kind,
            SiteKind::Dispatch,
            "a parallel for dispatches"
        );
        assert!(
            matches!(args.as_slice(), [Expr::Local(local)] if *local == item),
            "a parallel for dispatches its own item"
        );
        assert!(
            pushes_only(tail),
            "a parallel for's arms push values without effects"
        );
        out.push(Stmt::Parallel(ir::Parallel {
            item,
            list,
            bound: capacity,
            site: *site,
            args: args.clone(),
            result: *result,
            body: tail.to_vec(),
        }));
        Ok(false)
    }

    fn parallel_push(&self, statement: &ast::Stmt) -> Checked<()> {
        let ast::Stmt::Push { value, .. } = statement else {
            return parallel_error(statement, "the arms of a parallel for only push");
        };
        match self.effect_in(value) {
            Some(callee) => error(
                callee.at,
                format!(
                    "a push in a parallel for runs no effect: found a call of '{}'",
                    callee.text
                ),
            ),
            None => Ok(()),
        }
    }

    fn effect_in<'e>(&self, value: &'e ast::Expr) -> Option<&'e ast::Name> {
        match value {
            ast::Expr::Number(..) | ast::Expr::Text(..) | ast::Expr::Bool(..) => None,
            ast::Expr::Name(_) | ast::Expr::Take { .. } => None,
            ast::Expr::Member { base, .. } => self.effect_in(base),
            ast::Expr::Not { operand, .. } => self.effect_in(operand),
            ast::Expr::Binary { left, right, .. } => {
                self.effect_in(left).or_else(|| self.effect_in(right))
            }
            ast::Expr::Call { callee, args } => {
                let effect = self.globals.tools.contains_key(&callee.text)
                    || self.globals.model.as_deref() == Some(callee.text.as_str())
                    || EFFECT_BUILTINS.contains(&callee.text.as_str());
                if effect {
                    return Some(callee);
                }
                args.iter().find_map(|arg| self.effect_in(arg))
            }
        }
    }

    fn match_statement(
        &mut self,
        at: Position,
        subject: &ast::Expr,
        arms: &[ast::Arm],
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let subject_at = subject.at();
        let (subject, ty) = self.expr(subject, None, out)?;
        let Some(variants) = ty.variants(self.program) else {
            return error(
                subject_at,
                format!("'match' needs a sum type, found {}", self.type_name(&ty)),
            );
        };
        let text = self.program.text_enum(&ty).is_some();
        let mut checked: Vec<Option<(ir::Arm, bool)>> = vec![None; variants.len()];
        for arm in arms {
            let index = self.arm_index(&ty, &variants, &arm.pattern)?;
            let name = arm.pattern.name();
            if checked[index].is_some() {
                let shown = if text {
                    format!("value {}", quoted(&name.text))
                } else {
                    format!("variant '{}'", name.text)
                };
                return error(name.at, format!("{shown} is matched twice"));
            }
            let fields = &variants[index].fields;
            let empty = Vec::new();
            let names = match &arm.pattern {
                ast::Pattern::Variant { bindings, .. } => bindings,
                ast::Pattern::Value(_) => &empty,
            };
            if names.len() != fields.len() {
                return error(
                    name.at,
                    format!(
                        "variant '{}' has {} field(s), the pattern binds {}",
                        name.text,
                        fields.len(),
                        names.len()
                    ),
                );
            }
            let arm_checked = self.scoped(|checker| {
                let mut bindings = Vec::new();
                for (binding, field) in names.iter().zip(fields) {
                    bindings.push(if binding.text == WILDCARD {
                        None
                    } else {
                        Some(checker.declare(binding, field.clone())?)
                    });
                }
                let (body, returned) = checker.block(&arm.body)?;
                Ok((
                    ir::Arm {
                        variant: index,
                        bindings,
                        body,
                    },
                    returned,
                ))
            })?;
            checked[index] = Some(arm_checked);
        }
        let missing: Vec<&str> = variants
            .iter()
            .zip(&checked)
            .filter(|(_, arm)| arm.is_none())
            .map(|(variant, _)| variant.name.as_str())
            .collect();
        if text && !missing.is_empty() {
            let listed: Vec<String> = missing.iter().map(|value| quoted(value)).collect();
            return error(
                at,
                format!(
                    "match on {} is missing {}",
                    self.type_name(&ty),
                    listed.join(", ")
                ),
            );
        }
        if missing.contains(&ir::OUTCOME_VARIANTS[ir::UNKNOWN]) && matches!(ty, Type::Maybe(_)) {
            return error(
                at,
                format!(
                    "match on {} is missing 'unknown': an external effect whose result was not recorded has an unknown outcome",
                    self.type_name(&ty)
                ),
            );
        }
        if !missing.is_empty() {
            let listed = missing
                .iter()
                .map(|name| format!("'{name}'"))
                .collect::<Vec<_>>()
                .join(", ");
            return error(
                at,
                format!("match on {} is missing {listed}", self.type_name(&ty)),
            );
        }
        let mut all_return = true;
        let mut out_arms = Vec::new();
        for arm in checked {
            let (arm, returned) = arm.expect("every variant has an arm once none is missing");
            all_return &= returned;
            out_arms.push(arm);
        }
        out.push(Stmt::Match {
            subject,
            ty,
            arms: out_arms,
        });
        Ok(all_return)
    }

    fn arm_index(
        &self,
        ty: &Type,
        variants: &[ir::Variant],
        pattern: &ast::Pattern,
    ) -> Checked<usize> {
        let name = pattern.name();
        let position = variants
            .iter()
            .position(|variant| variant.name == name.text);
        match (pattern, self.program.text_enum(ty).is_some(), position) {
            (ast::Pattern::Value(_), true, Some(index))
            | (ast::Pattern::Variant { .. }, false, Some(index)) => Ok(index),
            (ast::Pattern::Value(_), true, None) => error(
                name.at,
                format!(
                    "{} is not a value of {}",
                    quoted(&name.text),
                    self.type_name(ty)
                ),
            ),
            (ast::Pattern::Variant { .. }, false, None) => error(
                name.at,
                format!("'{}' is not a variant of {}", name.text, self.type_name(ty)),
            ),
            (ast::Pattern::Variant { .. }, true, _) => error(
                name.at,
                format!(
                    "{} is a text enum: match its values as text, such as {}",
                    self.type_name(ty),
                    quoted(&variants[0].name)
                ),
            ),
            (ast::Pattern::Value(_), false, _) => error(
                name.at,
                format!(
                    "a text pattern matches a text enum, not {}",
                    self.type_name(ty)
                ),
            ),
        }
    }

    fn site(
        &mut self,
        kind: SiteKind,
        class: SiteClass,
        result: Type,
        args: Vec<Expr>,
        out: &mut Vec<Stmt>,
    ) -> (Expr, Type) {
        let site = self.sites.len();
        self.sites.push(Site {
            kind,
            class,
            result: result.clone(),
            durability: Durability::of(class),
        });
        let local = self.temporary(site, result.clone());
        out.push(Stmt::Effect {
            site,
            args,
            result: local,
        });
        (Expr::Local(local), result)
    }

    fn args(
        &mut self,
        at: Position,
        what: &str,
        args: &[ast::Expr],
        wanted: &[Type],
        out: &mut Vec<Stmt>,
    ) -> Checked<Vec<Expr>> {
        if args.len() != wanted.len() {
            return error(
                at,
                format!(
                    "{what} takes {} argument(s), found {}",
                    wanted.len(),
                    args.len()
                ),
            );
        }
        let mut checked = Vec::new();
        for (arg, wanted) in args.iter().zip(wanted) {
            let (value, ty) = self.expr(arg, Some(wanted), out)?;
            self.expect(arg.at(), wanted, &ty)?;
            checked.push(value);
        }
        Ok(checked)
    }

    fn call(
        &mut self,
        callee: &ast::Name,
        args: &[ast::Expr],
        expected: Option<&Type>,
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        let at = callee.at;
        if let Some(&tool_id) = self.globals.tools.get(&callee.text) {
            let tool = &self.program.tools[tool_id];
            let wanted: Vec<Type> = tool.params.iter().map(|param| param.ty.clone()).collect();
            let args = self.args(at, &format!("tool '{}'", callee.text), args, &wanted, out)?;
            let inner = Box::new(tool.returns.clone());
            let result = if tool.effect.may_be_unknown() {
                Type::Maybe(inner)
            } else {
                Type::Known(inner)
            };
            return Ok(self.site(
                SiteKind::Tool(tool_id),
                SiteClass::Fixed(tool.effect),
                result,
                args,
                out,
            ));
        }
        if let Some(&listing) = self.globals.listings.get(&callee.text) {
            self.args(at, &format!("listing '{}'", callee.text), args, &[], out)?;
            return Ok(self.site(
                SiteKind::Listing(listing),
                SiteClass::Fixed(EffectClass::None),
                Type::Known(Box::new(Type::U64)),
                Vec::new(),
                out,
            ));
        }
        if self.globals.model.as_deref() == Some(callee.text.as_str()) {
            let [context] = args else {
                return error(
                    at,
                    format!(
                        "model '{}' takes the context list as its one argument",
                        callee.text
                    ),
                );
            };
            let context_at = context.at();
            let (value, ty) = self.expr(context, None, out)?;
            let is_context =
                matches!(&ty, Type::List { element, .. } if **element == Type::Sum(ir::ENTRY));
            if !is_context {
                return error(
                    context_at,
                    format!(
                        "model '{}' reads a list<entry, _>, found {}",
                        callee.text,
                        self.type_name(&ty)
                    ),
                );
            }
            let result = self
                .program
                .model
                .as_ref()
                .expect("a model name is bound only with a model")
                .result();
            return Ok(self.site(
                SiteKind::Model,
                SiteClass::Fixed(EffectClass::Idempotent),
                result,
                vec![value],
                out,
            ));
        }
        match callee.text.as_str() {
            "len" => {
                let [list] = args else {
                    return error(at, "len takes one list");
                };
                let list_at = list.at();
                let (value, ty) = self.expr(list, None, out)?;
                if !matches!(ty, Type::List { .. }) {
                    return error(
                        list_at,
                        format!("len takes a list, found {}", self.type_name(&ty)),
                    );
                }
                Ok((Expr::Len(Box::new(value)), Type::U64))
            }
            "dispatch" => {
                let args = self.args(at, "dispatch", args, &[Type::Sum(ir::CALL)], out)?;
                Ok(self.site(
                    SiteKind::Dispatch,
                    SiteClass::Dispatched,
                    Type::Maybe(Box::new(Type::Text)),
                    args,
                    out,
                ))
            }
            "now" | "random" => {
                self.args(at, &callee.text, args, &[], out)?;
                let kind = if callee.text == "now" {
                    SiteKind::Now
                } else {
                    SiteKind::Random
                };
                Ok(self.site(
                    kind,
                    SiteClass::Fixed(EffectClass::None),
                    Type::U64,
                    Vec::new(),
                    out,
                ))
            }
            _ => self.construct(callee, args, expected, out),
        }
    }

    fn variant_of(
        &self,
        name: &ast::Name,
        expected: Option<&Type>,
    ) -> Checked<(Type, ir::VariantId, Vec<Type>)> {
        if let Some(expected) = expected
            && self.program.text_enum(expected).is_none()
            && let Some(variants) = expected.variants(self.program)
            && let Some(index) = variants
                .iter()
                .position(|variant| variant.name == name.text)
        {
            return Ok((expected.clone(), index, variants[index].fields.clone()));
        }
        let usable = |sum: usize| {
            let sum = &self.program.sums[sum];
            sum.kind == ir::SumKind::Tagged && !sum.variants.is_empty()
        };
        let mut candidates = Vec::new();
        for (sum_id, sum) in self
            .program
            .sums
            .iter()
            .enumerate()
            .filter(|(id, _)| usable(*id))
        {
            if let Some(index) = sum
                .variants
                .iter()
                .position(|variant| variant.name == name.text)
            {
                candidates.push((Type::Sum(sum_id), index, sum.variants[index].fields.clone()));
            }
        }
        let generic = ir::OUTCOME_VARIANTS.contains(&name.text.as_str());
        match (candidates.len(), generic) {
            (1, false) => Ok(candidates.remove(0)),
            (0, false) => error(name.at, format!("unknown name '{}'", name.text)),
            _ => {
                let mut owners: Vec<String> = candidates
                    .iter()
                    .map(|(ty, ..)| self.type_name(ty))
                    .collect();
                if generic {
                    owners.push("known<T>".to_owned());
                    owners.push("maybe<T>".to_owned());
                }
                error(
                    name.at,
                    format!(
                        "cannot tell which type '{}' builds here: it is a variant of {}",
                        name.text,
                        owners.join(", ")
                    ),
                )
            }
        }
    }

    fn construct(
        &mut self,
        name: &ast::Name,
        args: &[ast::Expr],
        expected: Option<&Type>,
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        let (ty, variant, fields) = self.variant_of(name, expected)?;
        let fields = self.args(
            name.at,
            &format!("variant '{}'", name.text),
            args,
            &fields,
            out,
        )?;
        Ok((
            Expr::Construct {
                ty: ty.clone(),
                variant,
                fields,
            },
            ty,
        ))
    }

    fn expr(
        &mut self,
        expr: &ast::Expr,
        expected: Option<&Type>,
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        match expr {
            ast::Expr::Number(value, _) => Ok((Expr::U64(*value), Type::U64)),
            ast::Expr::Text(text, at) => self.text(text, *at, expected),
            ast::Expr::Bool(value, _) => Ok((Expr::Bool(*value), Type::Bool)),
            ast::Expr::Name(name) => self.name(name, expected, out),
            ast::Expr::Member { base, field } => {
                let (base, ty) = self.expr(base, None, out)?;
                let index = ir::CALL_FIELDS.iter().position(|name| *name == field.text);
                match (&ty, index) {
                    (Type::Sum(ir::CALL), Some(index)) => Ok((
                        Expr::Member {
                            base: Box::new(base),
                            index,
                        },
                        Type::Text,
                    )),
                    _ => error(
                        field.at,
                        format!("{} has no field '{}'", self.type_name(&ty), field.text),
                    ),
                }
            }
            ast::Expr::Call { callee, args } => self.call(callee, args, expected, out),
            ast::Expr::Binary {
                at,
                op,
                left,
                right,
            } => self.binary(*at, *op, left, right, out),
            ast::Expr::Not { operand, .. } => {
                let operand_at = operand.at();
                let (operand, ty) = self.expr(operand, Some(&Type::Bool), out)?;
                self.expect(operand_at, &Type::Bool, &ty)?;
                Ok((Expr::Not(Box::new(operand)), Type::Bool))
            }
            ast::Expr::Take { at, events } => self.take(*at, events, out),
        }
    }

    fn text(&self, text: &str, at: Position, expected: Option<&Type>) -> Checked<(Expr, Type)> {
        let Some(sum) = expected.and_then(|expected| self.program.text_enum(expected)) else {
            return Ok((Expr::Text(text.to_owned()), Type::Text));
        };
        let ty = expected.expect("a text enum is expected").clone();
        let Some(variant) = sum.variants.iter().position(|value| value.name == text) else {
            return error(
                at,
                format!("{} is not a value of {}", quoted(text), sum.name),
            );
        };
        Ok((
            Expr::Construct {
                ty: ty.clone(),
                variant,
                fields: Vec::new(),
            },
            ty,
        ))
    }

    fn name(
        &mut self,
        name: &ast::Name,
        expected: Option<&Type>,
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        if self.program.events.cancel && name.text == CANCELLED_READ {
            self.events_readable(name.at, CANCELLED_READ)?;
            return Ok((Expr::Cancelled, Type::Bool));
        }
        if let Some(local) = self.lookup(&name.text) {
            return Ok((Expr::Local(local), self.locals[local].ty.clone()));
        }
        if let Some(&field) = self.globals.fields.get(&name.text) {
            return Ok((Expr::Field(field), self.program.state[field].ty.clone()));
        }
        if let Some(&constant) = self.globals.constants.get(&name.text) {
            return Ok((Expr::Constant(constant), Type::U64));
        }
        if self.globals.effect(&name.text) {
            return error(
                name.at,
                format!("'{}' is an effect: call it with arguments", name.text),
            );
        }
        self.construct(name, &[], expected, out)
    }

    fn binary(
        &mut self,
        at: Position,
        op: BinaryOp,
        left: &ast::Expr,
        right: &ast::Expr,
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        let (operand, result) = match op {
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply => (Some(Type::U64), Type::U64),
            BinaryOp::Concat => (Some(Type::Text), Type::Text),
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                (Some(Type::U64), Type::Bool)
            }
            BinaryOp::And | BinaryOp::Or => (Some(Type::Bool), Type::Bool),
            BinaryOp::Equal | BinaryOp::NotEqual => (None, Type::Bool),
        };
        let left_at = left.at();
        let right_at = right.at();
        let (left, left_type) = self.expr(left, operand.as_ref(), out)?;
        if operand.is_none() && !matches!(left_type, Type::U64 | Type::Bool | Type::Text) {
            return error(
                at,
                format!(
                    "'{}' compares u64, bool or text, not {}",
                    op.symbol(),
                    self.type_name(&left_type)
                ),
            );
        }
        let (right, right_type) =
            self.expr(right, Some(operand.as_ref().unwrap_or(&left_type)), out)?;
        let operand = match operand {
            Some(wanted) => {
                self.expect(left_at, &wanted, &left_type)?;
                self.expect(right_at, &wanted, &right_type)?;
                wanted
            }
            None => {
                self.expect(right_at, &left_type, &right_type)?;
                left_type
            }
        };
        Ok((
            Expr::Binary {
                op,
                operand,
                left: Box::new(left),
                right: Box::new(right),
            },
            result,
        ))
    }
}

const EFFECT_BUILTINS: [&str; 3] = ["dispatch", "now", "random"];

fn parallel_body<'b>(
    at: Position,
    name: &ast::Name,
    body: &'b ast::Block,
) -> Checked<(Position, &'b ast::Expr, &'b [ast::Arm])> {
    let shape = "the body of a parallel for is only a match on dispatch";
    let Some(first) = body.statements.first() else {
        return error(at, format!("{shape}({}): found nothing", name.text));
    };
    let ast::Stmt::Match {
        at: match_at,
        subject,
        arms,
    } = first
    else {
        return parallel_error(first, &format!("{shape}({})", name.text));
    };
    let dispatches_item = matches!(
        subject,
        ast::Expr::Call { callee, args }
            if callee.text == "dispatch"
                && matches!(args.as_slice(), [ast::Expr::Name(item)] if item.text == name.text)
    );
    if !dispatches_item {
        return error(
            subject.at(),
            format!(
                "a parallel for matches on dispatch({}), the item it walks",
                name.text
            ),
        );
    }
    if let Some(extra) = body.statements.get(1) {
        return parallel_error(extra, &format!("{shape}({})", name.text));
    }
    Ok((*match_at, subject, arms))
}

fn parallel_error<T>(statement: &ast::Stmt, shape: &str) -> Checked<T> {
    let at = statement_at(statement);
    let found = match statement {
        ast::Stmt::For { parallel: true, .. } => {
            return error(
                at,
                "a parallel for cannot be nested in another parallel for",
            );
        }
        ast::Stmt::Let { .. } => "a let".to_owned(),
        ast::Stmt::Assign { .. } => "an assignment".to_owned(),
        ast::Stmt::Push { .. } => "a push".to_owned(),
        ast::Stmt::If { .. } => "an if".to_owned(),
        ast::Stmt::Match { .. } => "a match".to_owned(),
        ast::Stmt::Loop { .. } => "a loop".to_owned(),
        ast::Stmt::For { .. } => "a for loop".to_owned(),
        ast::Stmt::Return { .. } => "a return".to_owned(),
        ast::Stmt::Call { callee, .. }
        | ast::Stmt::Inlined {
            function: callee, ..
        } => format!("a call of fn '{}'", callee.text),
    };
    error(at, format!("{shape}: found {found}"))
}

fn pushes_only(statements: &[Stmt]) -> bool {
    statements.iter().all(|statement| match statement {
        Stmt::Push { .. } => true,
        Stmt::Match { arms, .. } => arms.iter().all(|arm| pushes_only(&arm.body)),
        _ => false,
    })
}

pub(super) fn statement_at(statement: &ast::Stmt) -> Position {
    match statement {
        ast::Stmt::Let { name, .. } | ast::Stmt::Assign { name, .. } => name.at,
        ast::Stmt::Call { callee, .. } => callee.at,
        ast::Stmt::Inlined { at, .. } => *at,
        ast::Stmt::Push { at, .. }
        | ast::Stmt::If { at, .. }
        | ast::Stmt::Match { at, .. }
        | ast::Stmt::Loop { at, .. }
        | ast::Stmt::For { at, .. }
        | ast::Stmt::Return { at, .. } => *at,
    }
}
