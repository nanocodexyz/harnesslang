use super::{Checker, statement_at};
use crate::check::{ALLOW, Checked, DENY, RESPOND, error, resolve};
use crate::effect::EffectClass;
use crate::ir::{self, BinaryOp, Expr, LocalId, SiteClass, SiteKind, Stmt, Type};
use crate::syntax::Position;
use crate::syntax::ast::{self, HookKind};

#[derive(Clone, Copy)]
pub(super) enum Spawned {
    Direct,
    Dispatched(LocalId),
}

type Scopes = Vec<Vec<(String, LocalId)>>;

#[derive(Clone, Copy)]
struct Flags {
    decided: ir::FieldId,
    denied: ir::FieldId,
    responded: ir::FieldId,
    reason: ir::FieldId,
    input: ir::FieldId,
}

impl Flags {
    fn sent(self, original: LocalId) -> Expr {
        let member = |index| Expr::Member {
            base: Box::new(Expr::Local(original)),
            index,
        };
        Expr::Construct {
            ty: Type::Sum(ir::CALL),
            variant: 0,
            fields: vec![
                member(ir::CALL_ID),
                member(ir::CALL_TOOL),
                Expr::Field(self.input),
            ],
        }
    }
}

fn outcome_type() -> Type {
    Type::Maybe(Box::new(Type::Text))
}

impl Checker<'_> {
    pub(super) fn hidden(&mut self, name: &str, ty: Type) -> LocalId {
        let local = self.locals.len();
        self.locals.push(ir::Field {
            name: format!("_{name}{local}"),
            ty,
        });
        local
    }

    pub(super) fn opening(&mut self, params: &[LocalId], at: Position) -> Checked<Vec<Stmt>> {
        let mut out = Vec::new();
        if let Some(hook) = self.globals.hooks.get(&HookKind::SessionStart) {
            let field = self.globals.fields[ir::SESSION_STARTED_FIELD];
            let mut then = vec![Stmt::Assign {
                field,
                value: Expr::Bool(true),
            }];
            self.lifecycle_hook(hook, Vec::new(), at, &mut then)?;
            out.push(Stmt::If {
                condition: Expr::Not(Box::new(Expr::Field(field))),
                then,
                otherwise: Vec::new(),
            });
        }
        if let Some(hook) = self.globals.hooks.get(&HookKind::PromptSubmit) {
            let param = &hook.params[0];
            let taken = params
                .iter()
                .copied()
                .find(|local| self.locals[*local].name == param.name.text);
            if let Some(local) = taken {
                let wanted = resolve(self.program, self.globals, &param.ty)?;
                let ty = self.locals[local].ty.clone();
                if wanted != ty {
                    return error(
                        hook.at,
                        format!(
                            "'{}' takes '{}: {}', but a handler takes '{}: {}'",
                            hook.kind.describe(),
                            param.name.text,
                            self.type_name(&wanted),
                            param.name.text,
                            self.type_name(&ty)
                        ),
                    );
                }
                self.lifecycle_hook(hook, vec![(Expr::Local(local), ty)], at, &mut out)?;
            }
        }
        Ok(out)
    }

    pub(super) fn lifecycle(
        &mut self,
        kind: HookKind,
        args: Vec<(Expr, Type)>,
        at: Position,
        out: &mut Vec<Stmt>,
    ) -> Checked<()> {
        if self.in_hook || self.side.is_some() {
            return Ok(());
        }
        let Some(hook) = self.globals.hooks.get(&kind) else {
            return Ok(());
        };
        self.lifecycle_hook(hook, args, at, out)
    }

    pub(super) fn compaction(
        &mut self,
        kind: HookKind,
        compacts: bool,
        at: Position,
        out: &mut Vec<Stmt>,
    ) -> Checked<()> {
        if !compacts {
            return Ok(());
        }
        self.lifecycle(kind, Vec::new(), at, out)
    }

    fn lifecycle_hook(
        &mut self,
        hook: &ast::Hook,
        args: Vec<(Expr, Type)>,
        at: Position,
        out: &mut Vec<Stmt>,
    ) -> Checked<()> {
        self.used.insert(hook.kind);
        if self.hook_inline(hook, args, out)? {
            return error(
                at,
                format!(
                    "'{}' returns on every path, so what follows it here never runs",
                    hook.kind.describe()
                ),
            );
        }
        Ok(())
    }

    fn enter_hook(&mut self) -> (bool, Scopes) {
        let in_hook = std::mem::replace(&mut self.in_hook, true);
        let caller = std::mem::replace(&mut self.scopes, vec![Vec::new()]);
        (in_hook, caller)
    }

    fn leave_hook(&mut self, (in_hook, caller): (bool, Scopes)) {
        let own = std::mem::replace(&mut self.scopes, caller);
        assert_eq!(
            own.len(),
            1,
            "a hook body leaves only its parameters' scope"
        );
        self.in_hook = in_hook;
    }

    fn hook_params(
        &mut self,
        hook: &ast::Hook,
        args: Vec<(Expr, Type)>,
        out: &mut Vec<Stmt>,
    ) -> Checked<()> {
        assert_eq!(
            hook.params.len(),
            args.len(),
            "the checker gives a hook one argument per parameter"
        );
        for (param, (value, ty)) in hook.params.iter().zip(args) {
            let local = self.declare(&param.name, ty)?;
            out.push(Stmt::Let { local, value });
        }
        Ok(())
    }

    fn hook_inline(
        &mut self,
        hook: &ast::Hook,
        args: Vec<(Expr, Type)>,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let saved = self.enter_hook();
        let checked = self
            .hook_params(hook, args, out)
            .and_then(|()| self.block(&hook.body));
        self.leave_hook(saved);
        let (statements, returned) = checked?;
        out.extend(statements);
        Ok(returned)
    }

    pub(super) fn dispatch_hooks(&self) -> Vec<HookKind> {
        let mut kinds: Vec<HookKind> = [HookKind::BeforeTool, HookKind::AfterTool]
            .into_iter()
            .filter(|kind| self.globals.hooks.contains_key(kind))
            .collect();
        if self.starts_subagents() {
            kinds.push(HookKind::SubagentStart);
        }
        kinds
    }

    fn starts_subagents(&self) -> bool {
        self.globals.hooks.contains_key(&HookKind::SubagentStart)
            && self.program.spawns().next().is_some()
    }

    pub(super) fn plain_dispatch(&self, at: Position) -> Checked<()> {
        if self.in_hook {
            return error(
                at,
                "a hook cannot dispatch: a dispatch inside it would run without the hooks every dispatch runs",
            );
        }
        if let Some(kind) = self.dispatch_hooks().first() {
            return error(
                at,
                format!(
                    "match on dispatch(...) directly: '{}' runs at every dispatch and decides the arm its match takes",
                    kind.describe()
                ),
            );
        }
        Ok(())
    }

    pub(super) fn plain_spawn(&self, callee: &ast::Name) -> Checked<()> {
        if self.in_hook || self.spawn_subject || !self.starts_subagents() {
            return Ok(());
        }
        error(
            callee.at,
            format!(
                "match on {}(...) directly: 'on subagent start' runs in the ok arm of the session it started",
                callee.text
            ),
        )
    }

    pub(super) fn join(
        &mut self,
        at: Position,
        args: &[ast::Expr],
        out: &mut Vec<Stmt>,
    ) -> Checked<(Expr, Type)> {
        if self.program.spawns().next().is_none() {
            return error(
                at,
                "'join' waits for a spawned session, but the harness declares no spawn tool",
            );
        }
        if !self.program.events.resume {
            return error(
                at,
                format!(
                    "'join' needs 'on {}' on the harness: a child's completion resumes it",
                    crate::check::EVENT_RESUME
                ),
            );
        }
        let args = self.args(at, "join", args, &[Type::Text], out)?;
        let agent = args[0].clone();
        let (value, ty) = self.site(
            SiteKind::Suspend,
            SiteClass::Fixed(EffectClass::None),
            Type::Text,
            args,
            out,
        );
        self.lifecycle(
            HookKind::SubagentStop,
            vec![(agent, Type::Text), (value.clone(), Type::Text)],
            at,
            out,
        )?;
        Ok((value, ty))
    }

    pub(super) fn hooked_match(
        &mut self,
        at: Position,
        subject: &ast::Expr,
        arms: &[ast::Arm],
        out: &mut Vec<Stmt>,
    ) -> Checked<Option<bool>> {
        if self.in_hook || self.side.is_some() {
            return Ok(None);
        }
        let ast::Expr::Call {
            callee,
            args,
            mode: None,
            settings: None,
        } = subject
        else {
            return Ok(None);
        };
        if callee.text == "dispatch" && !self.dispatch_hooks().is_empty() {
            let [arg] = args.as_slice() else {
                return error(
                    callee.at,
                    format!("dispatch takes 1 argument(s), found {}", args.len()),
                );
            };
            return self.hooked_dispatch(at, arg, arms, out).map(Some);
        }
        let spawn = self
            .globals
            .tools
            .get(&callee.text)
            .is_some_and(|tool| self.program.tools[*tool].spawn.is_some());
        if !spawn || !self.starts_subagents() {
            return Ok(None);
        }
        let subject_at = subject.at();
        self.spawn_subject = true;
        let evaluated = self.expr(subject, None, out);
        self.spawn_subject = false;
        let subject = evaluated?;
        self.match_with(
            at,
            subject_at,
            subject,
            arms,
            &mut |checker, block| checker.block(block),
            Some(Spawned::Direct),
            out,
        )
        .map(Some)
    }

    fn hooked_dispatch(
        &mut self,
        at: Position,
        arg: &ast::Expr,
        arms: &[ast::Arm],
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let call_type = Type::Sum(ir::CALL);
        let arg_at = arg.at();
        let (call, ty) = self.expr(arg, Some(&call_type), out)?;
        self.expect(arg_at, &call_type, &ty)?;
        let original = self.hidden("call", call_type.clone());
        if self.lane_answer.is_some() {
            return self.lane_dispatch(at, original, call, arms, out);
        }
        out.push(Stmt::Let {
            local: original,
            value: call,
        });
        let before = self.globals.hooks.get(&HookKind::BeforeTool);
        let after = self.globals.hooks.get(&HookKind::AfterTool);
        if before.is_none() && after.is_none() {
            return self.dispatched(at, original, Expr::Local(original), arms, out);
        }
        let flags = self.flags();
        out.push(Stmt::Assign {
            field: flags.input,
            value: Expr::Member {
                base: Box::new(Expr::Local(original)),
                index: ir::CALL_INPUT,
            },
        });
        let Some(before) = before else {
            let sent = flags.sent(original);
            return self.dispatched(at, original, sent, arms, out);
        };
        self.decide(
            before,
            HookKind::BeforeTool,
            vec![(Expr::Local(original), call_type)],
            at,
            out,
        )?;
        let (denied, _) = self.site(
            SiteKind::Deny,
            SiteClass::Fixed(EffectClass::None),
            outcome_type(),
            vec![
                Expr::Local(original),
                Expr::Field(flags.reason),
                Expr::Field(flags.responded),
            ],
            out,
        );
        let denied_site = out.pop().expect("the deny site just added its effect");
        let mut refused = vec![denied_site];
        let refused_returns =
            self.outcome(at, (denied, outcome_type()), None, arms, &mut refused)?;
        let mut ran = Vec::new();
        let sent = flags.sent(original);
        let ran_returns = self.dispatched(at, original, sent, arms, &mut ran)?;
        out.push(Stmt::If {
            condition: Expr::Field(flags.denied),
            then: refused,
            otherwise: ran,
        });
        Ok(refused_returns && ran_returns)
    }

    pub(super) fn answered_parallel(&self, at: Position) -> Checked<()> {
        let refusal = |what: &str| {
            error(
                at,
                format!(
                    "a parallel for decides its calls before its batch, so '{}' cannot {what}: use a sequential for",
                    HookKind::BeforeTool.describe()
                ),
            )
        };
        if self.globals.hooks.contains_key(&HookKind::AfterTool) {
            return refusal(&format!(
                "share it with '{}'",
                HookKind::AfterTool.describe()
            ));
        }
        if self.starts_subagents() {
            return refusal("decide a subagent's start");
        }
        let hook = &self.globals.hooks[&HookKind::BeforeTool];
        if allows(&hook.body.statements) {
            return refusal("rewrite a call with 'allow'");
        }
        Ok(())
    }

    fn lane_dispatch(
        &mut self,
        at: Position,
        original: LocalId,
        call: Expr,
        arms: &[ast::Arm],
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let call_type = Type::Sum(ir::CALL);
        let flags = self.flags();
        let mut prelude = vec![
            Stmt::Let {
                local: original,
                value: call.clone(),
            },
            Stmt::Assign {
                field: flags.input,
                value: Expr::Member {
                    base: Box::new(Expr::Local(original)),
                    index: ir::CALL_INPUT,
                },
            },
        ];
        let before = self.globals.hooks[&HookKind::BeforeTool].clone();
        self.decide(
            &before,
            HookKind::BeforeTool,
            vec![(Expr::Local(original), call_type)],
            at,
            &mut prelude,
        )?;
        if ir::runs_effects(&prelude) {
            return error(
                at,
                format!(
                    "a parallel for decides each call before its batch, so its '{}' block cannot run an effect: use a sequential for",
                    HookKind::BeforeTool.describe()
                ),
            );
        }
        let args = vec![
            Expr::Local(original),
            Expr::Field(flags.reason),
            Expr::Field(flags.responded),
        ];
        let result_type = self.program.dispatch_result();
        let mut parked = Vec::new();
        self.site(
            SiteKind::Deny,
            SiteClass::Fixed(EffectClass::None),
            result_type.clone(),
            args.clone(),
            &mut parked,
        );
        let [Stmt::Effect { site, .. }] = parked.as_slice() else {
            unreachable!("a site adds one effect");
        };
        self.lane_answer = Some(Some(ir::Answer {
            prelude,
            decided: Expr::Field(flags.denied),
            site: *site,
            args,
        }));
        let (result, ty) = self.dispatch_site(result_type, vec![call], out);
        self.outcome(at, (result, ty), None, arms, out)
    }

    fn flags(&self) -> Flags {
        let field = |name: &str| self.globals.fields[name];
        Flags {
            decided: field(ir::HOOK_DECIDED_FIELD),
            denied: field(ir::HOOK_DENIED_FIELD),
            responded: field(ir::HOOK_RESPONDED_FIELD),
            reason: field(ir::HOOK_REASON_FIELD),
            input: field(ir::HOOK_INPUT_FIELD),
        }
    }

    fn decide(
        &mut self,
        hook: &ast::Hook,
        kind: HookKind,
        args: Vec<(Expr, Type)>,
        at: Position,
        out: &mut Vec<Stmt>,
    ) -> Checked<()> {
        self.used.insert(kind);
        let flags = self.flags();
        for field in [flags.decided, flags.denied, flags.responded] {
            out.push(Stmt::Assign {
                field,
                value: Expr::Bool(false),
            });
        }
        let saved = self.enter_hook();
        let decided = self
            .hook_params(hook, args, out)
            .and_then(|()| self.guarded(&hook.body.statements, kind, &flags, out));
        self.leave_hook(saved);
        if decided? {
            return error(
                at,
                format!(
                    "'{}' returns on every path, so the call it decides never runs",
                    kind.describe()
                ),
            );
        }
        Ok(())
    }

    fn dispatched(
        &mut self,
        at: Position,
        original: LocalId,
        sent: Expr,
        arms: &[ast::Arm],
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let (result, ty) = self.dispatch_site(outcome_type(), vec![sent], out);
        let spawned = Some(Spawned::Dispatched(original));
        let Some(hook) = self.globals.hooks.get(&HookKind::AfterTool) else {
            return self.outcome(at, (result, ty), spawned, arms, out);
        };
        let flags = self.flags();
        let args = vec![
            (Expr::Local(original), Type::Sum(ir::CALL)),
            (result.clone(), outcome_type()),
        ];
        self.decide(hook, HookKind::AfterTool, args, at, out)?;
        let failed = self.hidden("denied", outcome_type());
        let mut withheld = vec![Stmt::Let {
            local: failed,
            value: Expr::Construct {
                ty: outcome_type(),
                variant: ir::FAILED,
                fields: vec![Expr::Field(flags.reason)],
            },
        }];
        let withheld_returns = self.outcome(
            at,
            (Expr::Local(failed), outcome_type()),
            None,
            arms,
            &mut withheld,
        )?;
        let mut kept = Vec::new();
        let kept_returns = self.outcome(at, (result, ty), spawned, arms, &mut kept)?;
        out.push(Stmt::If {
            condition: Expr::Field(flags.denied),
            then: withheld,
            otherwise: kept,
        });
        Ok(withheld_returns && kept_returns)
    }

    fn outcome(
        &mut self,
        at: Position,
        subject: (Expr, Type),
        spawned: Option<Spawned>,
        arms: &[ast::Arm],
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        let spawned = spawned.filter(|_| self.starts_subagents());
        self.match_with(
            at,
            at,
            subject,
            arms,
            &mut |checker, block| checker.block(block),
            spawned,
            out,
        )
    }

    fn guarded(
        &mut self,
        statements: &[ast::Stmt],
        kind: HookKind,
        flags: &Flags,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        self.scoped(|checker| {
            for (index, statement) in statements.iter().enumerate() {
                let rest = &statements[index + 1..];
                if let ast::Stmt::Call { callee, args } = statement
                    && (callee.text == DENY || callee.text == ALLOW || callee.text == RESPOND)
                {
                    if let Some(next) = rest.first() {
                        return error(
                            statement_at(next),
                            format!(
                                "statement is unreachable after '{}', which decides the call",
                                callee.text
                            ),
                        );
                    }
                    checker.verdict(callee, args, kind, flags, out)?;
                    return Ok(false);
                }
                if decides(statement)? {
                    let returned = checker.decide_compound(statement, kind, flags, out)?;
                    if returned {
                        if let Some(next) = rest.first() {
                            return error(
                                statement_at(next),
                                "statement is unreachable after a return",
                            );
                        }
                        return Ok(true);
                    }
                    if rest.is_empty() {
                        return Ok(false);
                    }
                    let mut undecided = Vec::new();
                    checker.guarded(rest, kind, flags, &mut undecided)?;
                    out.push(Stmt::If {
                        condition: Expr::Not(Box::new(Expr::Field(flags.decided))),
                        then: undecided,
                        otherwise: Vec::new(),
                    });
                    return Ok(false);
                }
                if checker.statement(statement, out)? {
                    if let Some(next) = rest.first() {
                        return error(
                            statement_at(next),
                            "statement is unreachable after a return",
                        );
                    }
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }

    fn decide_compound(
        &mut self,
        statement: &ast::Stmt,
        kind: HookKind,
        flags: &Flags,
        out: &mut Vec<Stmt>,
    ) -> Checked<bool> {
        match statement {
            ast::Stmt::If {
                condition,
                then,
                otherwise,
                ..
            } => {
                let at = condition.at();
                let (condition, ty) = self.expr(condition, Some(&Type::Bool), out)?;
                self.expect(at, &Type::Bool, &ty)?;
                let mut then_out = Vec::new();
                let then_returns = self.guarded(&then.statements, kind, flags, &mut then_out)?;
                let mut otherwise_out = Vec::new();
                let rest: &[ast::Stmt] = otherwise.as_ref().map_or(&[], |block| &block.statements);
                let otherwise_returns = self.guarded(rest, kind, flags, &mut otherwise_out)?;
                out.push(Stmt::If {
                    condition,
                    then: then_out,
                    otherwise: otherwise_out,
                });
                Ok(then_returns && otherwise_returns)
            }
            ast::Stmt::Match { at, subject, arms } => {
                let subject_at = subject.at();
                let subject = self.expr(subject, None, out)?;
                self.match_with(
                    *at,
                    subject_at,
                    subject,
                    arms,
                    &mut |checker, block| {
                        let mut statements = Vec::new();
                        let returned =
                            checker.guarded(&block.statements, kind, flags, &mut statements)?;
                        Ok((statements, returned))
                    },
                    None,
                    out,
                )
            }
            _ => panic!("only an if or a match holds a verdict, as decides found"),
        }
    }

    fn verdict(
        &mut self,
        callee: &ast::Name,
        args: &[ast::Expr],
        kind: HookKind,
        flags: &Flags,
        out: &mut Vec<Stmt>,
    ) -> Checked<()> {
        let [arg] = args else {
            panic!("the inliner gives '{}' one argument", callee.text);
        };
        let at = arg.at();
        let (value, ty) = self.expr(arg, Some(&Type::Text), out)?;
        self.expect(at, &Type::Text, &ty)?;
        let target = if callee.text == DENY || callee.text == RESPOND {
            let mut fields = vec![flags.denied];
            if callee.text == RESPOND {
                assert_eq!(
                    kind,
                    HookKind::BeforeTool,
                    "the inliner admits 'respond' only in a before tool block"
                );
                fields.push(flags.responded);
            }
            for field in fields {
                out.push(Stmt::Assign {
                    field,
                    value: Expr::Bool(true),
                });
            }
            flags.reason
        } else {
            assert_eq!(
                kind,
                HookKind::BeforeTool,
                "the inliner admits 'allow' only in a before tool block"
            );
            flags.input
        };
        out.push(Stmt::Assign {
            field: target,
            value,
        });
        out.push(Stmt::Assign {
            field: flags.decided,
            value: Expr::Bool(true),
        });
        Ok(())
    }

    pub(super) fn spawned(
        &mut self,
        spawned: Spawned,
        agent: LocalId,
        at: Position,
        prelude: &mut Vec<Stmt>,
    ) -> Checked<()> {
        let Some(hook) = self.globals.hooks.get(&HookKind::SubagentStart) else {
            return Ok(());
        };
        let args = vec![(Expr::Local(agent), Type::Text)];
        let Spawned::Dispatched(call) = spawned else {
            return self.lifecycle_hook(hook, args, at, prelude);
        };
        let condition = self
            .program
            .spawns()
            .map(|(tool, _)| Expr::Binary {
                op: BinaryOp::Equal,
                operand: Type::Text,
                left: Box::new(Expr::Member {
                    base: Box::new(Expr::Local(call)),
                    index: ir::CALL_TOOL,
                }),
                right: Box::new(Expr::Text(tool.name.clone())),
            })
            .reduce(|left, right| Expr::Binary {
                op: BinaryOp::Or,
                operand: Type::Bool,
                left: Box::new(left),
                right: Box::new(right),
            })
            .expect("a harness that starts subagents declares a spawn tool");
        let mut then = Vec::new();
        self.lifecycle_hook(hook, args, at, &mut then)?;
        prelude.push(Stmt::If {
            condition,
            then,
            otherwise: Vec::new(),
        });
        Ok(())
    }
}

fn allows(statements: &[ast::Stmt]) -> bool {
    statements.iter().any(|statement| match statement {
        ast::Stmt::Call { callee, .. } => callee.text == ALLOW,
        ast::Stmt::If {
            then, otherwise, ..
        } => {
            allows(&then.statements)
                || otherwise
                    .as_ref()
                    .is_some_and(|block| allows(&block.statements))
        }
        ast::Stmt::Match { arms, .. } => arms.iter().any(|arm| allows(&arm.body.statements)),
        _ => false,
    })
}

fn decides(statement: &ast::Stmt) -> Checked<bool> {
    let any = |statements: &[ast::Stmt]| -> Checked<bool> {
        for statement in statements {
            if decides(statement)? {
                return Ok(true);
            }
        }
        Ok(false)
    };
    match statement {
        ast::Stmt::Call { callee, .. } => {
            Ok(callee.text == DENY || callee.text == ALLOW || callee.text == RESPOND)
        }
        ast::Stmt::If {
            then, otherwise, ..
        } => Ok(any(&then.statements)?
            || otherwise
                .as_ref()
                .map_or(Ok(false), |block| any(&block.statements))?),
        ast::Stmt::Match { arms, .. } => {
            for arm in arms {
                if any(&arm.body.statements)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        ast::Stmt::Loop { body, .. } | ast::Stmt::For { body, .. } => {
            if any(&body.statements)? {
                return error(
                    statement_at(statement),
                    "a 'deny', 'allow' or 'respond' decides the call once, so it cannot sit inside a loop",
                );
            }
            Ok(false)
        }
        ast::Stmt::Inlined { body, .. } => any(&body.statements),
        ast::Stmt::Climb(_) | ast::Stmt::Yield { .. } => {
            panic!("climbs are expanded before hooks are read")
        }
        ast::Stmt::Let { .. }
        | ast::Stmt::Assign { .. }
        | ast::Stmt::Push { .. }
        | ast::Stmt::Clear { .. }
        | ast::Stmt::Keep { .. }
        | ast::Stmt::KeepBlobs { .. }
        | ast::Stmt::Rewrite { .. }
        | ast::Stmt::Return { .. } => Ok(false),
    }
}
