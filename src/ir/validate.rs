use crate::effect::EffectClass;
use crate::ir::{
    self, Durability, Expr, Handler, Kept, Program, SiteClass, SiteKind, Stmt, SumKind, Type,
};

pub fn program(program: &Program) {
    assert!(
        program.sums.len() >= crate::ir::BUILTIN_SUMS,
        "every program carries the builtin sums"
    );
    for code in &program.codes {
        assert!(!code.tools.is_empty(), "a code tool has nested tools");
        assert!(
            code.parallel.value <= code.calls.value,
            "a code tool runs at most its calls at once"
        );
        for tool in &code.tools {
            assert!(*tool < program.tools.len(), "a nested tool is declared");
        }
    }
    for invariant in &program.invariants {
        assert!(
            !reads_local(&invariant.condition),
            "an invariant reads only state and constants"
        );
        assert!(
            !reads_events(&invariant.condition),
            "an invariant reads no events"
        );
    }
    reply(program);
    listings(program);
    for handler in &program.handlers {
        self::handler(program, handler);
    }
    for side in &program.sides {
        self::handler(program, side);
        side_reads_only(side);
    }
    if let Some(forked) = &program.forked {
        self::handler(program, forked);
        assert!(forked.sites.is_empty(), "on forked runs no effect");
        assert!(forked.requires.is_none(), "on forked has no requires");
    }
    for (id, function) in program.functions.iter().enumerate() {
        self::handler(program, function);
        assert!(function.sites.is_empty(), "a pure fn runs no effect");
        assert!(!writes_state(&function.body), "a pure fn changes no state");
        assert!(
            calls_below(&function.body, id),
            "a pure fn calls only the fns declared above it"
        );
    }
}

fn calls_below(statements: &[Stmt], id: usize) -> bool {
    let mut below = true;
    ir::visit_exprs(statements, &mut |value| {
        if let Expr::Call { function, .. } = value {
            below &= *function < id;
        }
    });
    below
}

fn side_reads_only(side: &ir::Handler) {
    for site in &side.sites {
        let reads = match (site.kind, site.class) {
            (ir::SiteKind::Model, _) => true,
            (ir::SiteKind::Tool(_), ir::SiteClass::Fixed(class)) => class == EffectClass::None,
            _ => false,
        };
        assert!(
            reads,
            "a side handler calls only the model and tools of effect none"
        );
    }
    assert!(
        !writes_state(&side.body),
        "a side handler changes no state field"
    );
}

fn writes_state(statements: &[ir::Stmt]) -> bool {
    statements.iter().any(|statement| match statement {
        ir::Stmt::Assign { .. }
        | ir::Stmt::Push { .. }
        | ir::Stmt::Shrink { .. }
        | ir::Stmt::KeepBlobs { .. }
        | ir::Stmt::Rewrite { .. }
        | ir::Stmt::Take { .. }
        | ir::Stmt::Parallel(_) => true,
        ir::Stmt::If {
            then, otherwise, ..
        } => writes_state(then) || writes_state(otherwise),
        ir::Stmt::Match { arms, .. } => arms.iter().any(|arm| writes_state(&arm.body)),
        ir::Stmt::Loop { body, .. } | ir::Stmt::For { body, .. } | ir::Stmt::While { body, .. } => {
            writes_state(body)
        }
        ir::Stmt::Let { .. }
        | ir::Stmt::Effect { .. }
        | ir::Stmt::Finish { .. }
        | ir::Stmt::Set { .. }
        | ir::Stmt::SetIndex { .. }
        | ir::Stmt::PushLocal { .. } => false,
    })
}

fn listings(program: &Program) {
    for (id, listing) in program.listings.iter().enumerate() {
        assert!(
            !listing.name.contains(ir::LISTED_TOOL_SEPARATOR),
            "a listing's name holds no separator, so a listed tool names one listing"
        );
        let probe = format!("{}{}tool", listing.name, ir::LISTED_TOOL_SEPARATOR);
        assert_eq!(
            program.listed(&probe).map(|(found, _)| found),
            Some(id),
            "a listing's tools are named by its prefix"
        );
        assert!(
            program.tool(&listing.name).is_none(),
            "a listing's name is not a tool's"
        );
    }
    for tool in &program.tools {
        assert!(
            program.listed(&tool.name).is_none(),
            "a declared tool is not named like a listed one"
        );
    }
}

fn reply(program: &Program) {
    let Some(model) = &program.model else {
        return;
    };
    let sum = &program.sums[ir::REPLY];
    assert_eq!(sum.kind, SumKind::Tagged, "a reply is a tagged sum");
    let calls = Type::List {
        element: Box::new(Type::Sum(ir::CALL)),
        capacity: model.calls.clone(),
    };
    let carriers: Vec<(usize, usize)> = sum
        .variants
        .iter()
        .enumerate()
        .flat_map(|(variant, definition)| {
            definition
                .fields
                .iter()
                .enumerate()
                .filter(|(_, field)| **field == calls)
                .map(move |(field, _)| (variant, field))
        })
        .collect();
    assert_eq!(
        carriers,
        vec![model.calls_at()],
        "exactly one reply variant carries the model's list of calls, where the model says"
    );
    assert!(
        !model.declared()
            || sum
                .variants
                .iter()
                .all(|variant| variant.name != ir::MODEL_FAILED),
        "a declared reply has no variant named like a failed model call"
    );
    if let Some(window) = &model.window {
        let stop = &program.sums[window.stop];
        assert_eq!(stop.kind, SumKind::Text, "stop is a text enum");
        assert!(
            stop.variants
                .iter()
                .map(|variant| variant.name.as_str())
                .eq(ir::STOP_VALUES),
            "stop holds the stop reasons in order"
        );
        assert_eq!(
            program.sums[window.attempt].variants,
            ir::attempt_variants(window.stop, model.interrupts.any()),
            "an attempt is ok with reply, stop and usage, or one of the model errors"
        );
        assert!(window.tokens.value > 0, "a context window holds tokens");
    }
}

fn reads_local(value: &Expr) -> bool {
    match value {
        Expr::U64(_)
        | Expr::Bool(_)
        | Expr::Text(_)
        | Expr::Constant(_)
        | Expr::Field(_)
        | Expr::ListingNotice(_) => false,
        Expr::Local(_) => true,
        Expr::Member { base, .. } => reads_local(base),
        Expr::Construct { fields, .. } => fields.iter().any(reads_local),
        Expr::Binary { left, right, .. }
        | Expr::With {
            list: left,
            entry: right,
        }
        | Expr::Index {
            base: left,
            index: right,
        } => reads_local(left) || reads_local(right),
        Expr::Copy { value, .. } => reads_local(value),
        Expr::Call { args, .. } | Expr::Bytes { args, .. } => args.iter().any(reads_local),
        Expr::Not(operand)
        | Expr::Len(operand)
        | Expr::TextLen(operand)
        | Expr::Truncate { text: operand, .. } => reads_local(operand),
        Expr::TextBuiltin { args, .. } => args.iter().any(reads_local),
        Expr::Outcome {
            tag,
            value,
            reason,
            original,
            ..
        } => [tag, value, reason, original]
            .into_iter()
            .any(|part| reads_local(part)),
        Expr::Cancelled => false,
    }
}

fn reads_events(value: &Expr) -> bool {
    match value {
        Expr::U64(_)
        | Expr::Bool(_)
        | Expr::Text(_)
        | Expr::Constant(_)
        | Expr::Field(_)
        | Expr::ListingNotice(_)
        | Expr::Local(_) => false,
        Expr::Cancelled => true,
        Expr::Member { base, .. } => reads_events(base),
        Expr::Construct { fields, .. } => fields.iter().any(reads_events),
        Expr::Binary { left, right, .. }
        | Expr::With {
            list: left,
            entry: right,
        }
        | Expr::Index {
            base: left,
            index: right,
        } => reads_events(left) || reads_events(right),
        Expr::Copy { value, .. } => reads_events(value),
        Expr::Call { args, .. } | Expr::Bytes { args, .. } => args.iter().any(reads_events),
        Expr::Not(operand)
        | Expr::Len(operand)
        | Expr::TextLen(operand)
        | Expr::Truncate { text: operand, .. } => reads_events(operand),
        Expr::TextBuiltin { args, .. } => args.iter().any(reads_events),
        Expr::Outcome {
            tag,
            value,
            reason,
            original,
            ..
        } => [tag, value, reason, original]
            .into_iter()
            .any(|part| reads_events(part)),
    }
}

fn handler(program: &Program, handler: &Handler) {
    let mut used = vec![0_u32; handler.sites.len()];
    for site in &handler.sites {
        assert_eq!(
            site.durability,
            Durability::of_site(site.kind, site.class),
            "a site's durability follows from its kind and class"
        );
        if site.kind.parks() {
            assert_eq!(
                site.class,
                SiteClass::Fixed(EffectClass::None),
                "a wait or suspend is a none effect"
            );
            assert!(
                program.events.resume || site.kind != SiteKind::Suspend,
                "a suspend needs the resume event"
            );
        }
        if let Some(constant) = site.kind.sleeps() {
            assert!(
                constant < program.constants.len(),
                "a wait names a constant"
            );
            assert_eq!(
                site.result,
                Type::U64,
                "a wait yields the clock at its wake"
            );
        }
        if site.kind == SiteKind::Suspend {
            assert_eq!(
                site.result,
                Type::Text,
                "a suspend yields the resume's answer"
            );
        }
        if let SiteKind::Tool(tool) = site.kind {
            assert_eq!(
                site.class,
                SiteClass::Fixed(program.tools[tool].effect),
                "a tool site has its tool's class"
            );
        }
        if let SiteKind::Cell(code) = site.kind {
            assert!(code < program.codes.len(), "a cell site names a code tool");
            assert_eq!(
                site.class,
                SiteClass::Cell,
                "a cell site has the cell class"
            );
            assert_eq!(
                site.result,
                Type::Maybe(Box::new(Type::Text)),
                "a cell yields maybe<text>"
            );
        }
        if let SiteKind::Listing(listing) = site.kind {
            assert!(
                listing < program.listings.len(),
                "a listing site reads a declared listing"
            );
            assert_eq!(
                site.class,
                SiteClass::Fixed(EffectClass::None),
                "reading a listing is a none effect"
            );
            assert_eq!(
                site.result,
                Type::Known(Box::new(Type::U64)),
                "a listing site yields known<u64>, the tools it admitted"
            );
        }
        if site.kind == SiteKind::Model {
            let model = program.model.as_ref().expect("a model site needs a model");
            assert_eq!(
                site.result,
                model.result(),
                "a model site yields the model's result"
            );
        }
        if site.kind == SiteKind::Dispatch {
            assert_eq!(
                site.class,
                SiteClass::Dispatched,
                "a dispatch resolves its class at run time"
            );
            assert_eq!(
                site.result,
                program.dispatch_result(),
                "dispatch yields maybe<text>, or dispatched when a tool returns a blob"
            );
        }
    }
    for param in &handler.params {
        assert!(*param < handler.locals.len(), "a parameter is a local");
    }
    if let Some(requires) = &handler.requires {
        expr(program, handler, requires);
        assert!(!reads_events(requires), "a requires reads no events");
    }
    block(program, handler, &handler.body, &mut used);
    for (site, count) in used.iter().enumerate() {
        assert_eq!(
            *count, 1,
            "site {site} of {} is placed exactly once",
            handler.name
        );
    }
}

fn block(program: &Program, handler: &Handler, statements: &[Stmt], used: &mut [u32]) {
    for statement in statements {
        match statement {
            Stmt::Let { local, value } => {
                assert!(*local < handler.locals.len(), "a let binds a local");
                expr(program, handler, value);
            }
            Stmt::Assign { field, value } | Stmt::Push { field, value } => {
                assert!(
                    *field < program.state.len(),
                    "assignments target state fields"
                );
                expr(program, handler, value);
            }
            Stmt::Rewrite { field, arms } => {
                let Type::List { element, .. } = &program.state[*field].ty else {
                    panic!("a rewrite targets a list field");
                };
                let variants = element
                    .variants(program)
                    .expect("a rewrite maps a list of a sum");
                assert_eq!(
                    arms.len(),
                    variants.len(),
                    "a rewrite has one arm per variant"
                );
                for (index, arm) in arms.iter().enumerate() {
                    assert_eq!(arm.variant, index, "rewrite arms are in variant order");
                    if let ir::Rewritten::Into(entry) = &arm.entry {
                        expr(program, handler, entry);
                    }
                }
            }
            Stmt::KeepBlobs { field, .. } => {
                assert!(
                    program.blobs.is_some(),
                    "a program that keeps blobs has blobs"
                );
                assert!(
                    matches!(&program.state[*field].ty, Type::List { element, .. } if **element == Type::Sum(ir::ENTRY)),
                    "keep blobs targets a context"
                );
            }
            Stmt::Shrink { field, kept } => {
                assert!(
                    matches!(program.state[*field].ty, Type::List { .. }),
                    "clear and keep target a list field"
                );
                match kept {
                    Kept::Nothing => {}
                    Kept::Last(count) => expr(program, handler, count),
                    Kept::LastAfter { count, first } => {
                        expr(program, handler, count);
                        expr(program, handler, first);
                    }
                }
            }
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                expr(program, handler, condition);
                block(program, handler, then, used);
                block(program, handler, otherwise, used);
            }
            Stmt::Match { subject, ty, arms } => {
                expr(program, handler, subject);
                let variants = ty.variants(program).expect("a match subject is a sum");
                assert_eq!(
                    arms.len(),
                    variants.len(),
                    "a match has one arm per variant"
                );
                for (index, arm) in arms.iter().enumerate() {
                    assert_eq!(arm.variant, index, "arms are in variant order");
                    assert_eq!(
                        arm.bindings.len(),
                        variants[index].fields.len(),
                        "an arm binds each field of its variant"
                    );
                    block(program, handler, &arm.body, used);
                }
            }
            Stmt::Loop { body, bound, .. }
            | Stmt::For { body, bound, .. }
            | Stmt::While { body, bound, .. } => {
                if let Stmt::While { condition, .. } = statement {
                    expr(program, handler, condition);
                }
                assert!(!bound.name.is_empty(), "every loop has a named bound");
                block(program, handler, body, used);
            }
            Stmt::Parallel(parallel) => {
                assert!(
                    !parallel.bound.name.is_empty(),
                    "every loop has a named bound"
                );
                assert_eq!(
                    handler.sites[parallel.site].kind,
                    SiteKind::Dispatch,
                    "a parallel for dispatches"
                );
                assert!(
                    matches!(parallel.args.as_slice(), [Expr::Local(item)] if *item == parallel.item),
                    "a parallel for dispatches its own item"
                );
                assert!(
                    lane_valid(handler, &parallel.body),
                    "the arms of a parallel for neither return, dispatch, read events nor nest a batch"
                );
                expr(program, handler, &parallel.list);
                block(
                    program,
                    handler,
                    std::slice::from_ref(&parallel.dispatch()),
                    used,
                );
                block(program, handler, &parallel.body, used);
                if let Some(answer) = &parallel.answer {
                    assert_eq!(
                        handler.sites[answer.site].kind,
                        SiteKind::Deny,
                        "a parallel for answers through a deny site"
                    );
                    assert!(
                        !crate::ir::runs_effects(&answer.prelude),
                        "an answer's prelude runs no effect"
                    );
                    block(program, handler, &answer.prelude, used);
                    expr(program, handler, &answer.decided);
                    block(
                        program,
                        handler,
                        &[Stmt::Effect {
                            site: answer.site,
                            args: answer.args.clone(),
                            result: parallel.result,
                        }],
                        used,
                    );
                }
            }
            Stmt::Effect { site, args, result } => {
                assert!(*result < handler.locals.len(), "an effect binds a local");
                assert_eq!(
                    handler.locals[*result].ty, handler.sites[*site].result,
                    "an effect's local has its site's result type"
                );
                used[*site] += 1;
                args.iter().for_each(|arg| expr(program, handler, arg));
            }
            Stmt::Take { result, taken } => {
                assert!(*result < handler.locals.len(), "a take binds a local");
                assert_eq!(
                    Some(&handler.locals[*result].ty),
                    program.events.taken_type(*taken).as_ref(),
                    "a take binds a list of the declared events"
                );
            }
            Stmt::Finish { value } => expr(program, handler, value),
            Stmt::Set { local, value } => {
                assert!(*local < handler.locals.len(), "a set targets a local");
                expr(program, handler, value);
            }
            Stmt::SetIndex {
                local,
                index,
                value,
            } => {
                assert!(
                    matches!(
                        handler.locals[*local].ty,
                        Type::List { .. } | Type::Bytes(_)
                    ),
                    "an indexed set targets a list or bytes local"
                );
                expr(program, handler, index);
                expr(program, handler, value);
            }
            Stmt::PushLocal { local, value, .. } => {
                assert!(
                    matches!(
                        handler.locals[*local].ty,
                        Type::List { .. } | Type::Bytes(_)
                    ),
                    "a local push targets a list or bytes local"
                );
                expr(program, handler, value);
            }
        }
    }
}

fn presents_payment(args: &[Expr]) -> bool {
    let [Expr::Construct { fields, .. }] = args else {
        return false;
    };
    matches!(
        fields.get(ir::CALL_INPUT),
        Some(Expr::TextBuiltin {
            op: ir::TextOp::WithPayment,
            ..
        })
    )
}

fn lane_valid(handler: &Handler, statements: &[Stmt]) -> bool {
    statements.iter().all(|statement| match statement {
        Stmt::Finish { .. } | Stmt::Parallel(_) | Stmt::Take { .. } => false,
        Stmt::Effect { site, args, .. } => {
            handler.sites[*site].kind != SiteKind::Dispatch || presents_payment(args)
        }
        Stmt::If {
            then, otherwise, ..
        } => lane_valid(handler, then) && lane_valid(handler, otherwise),
        Stmt::Match { arms, .. } => arms.iter().all(|arm| lane_valid(handler, &arm.body)),
        Stmt::Loop { body, .. } | Stmt::For { body, .. } => lane_valid(handler, body),
        _ => true,
    })
}

fn expr(program: &Program, handler: &Handler, value: &Expr) {
    match value {
        Expr::Cancelled => assert!(
            program.events.cancel,
            "only a harness with 'on cancel' reads it"
        ),
        Expr::U64(_) | Expr::Bool(_) | Expr::Text(_) | Expr::Constant(_) | Expr::Field(_) => {}
        Expr::ListingNotice(listing) => {
            assert!(
                *listing < program.listings.len(),
                "a notice reads a declared listing"
            );
            assert!(
                program.notice.is_some(),
                "a notice is an entry of a program with a listing"
            );
        }
        Expr::Local(local) => assert!(*local < handler.locals.len(), "a local exists"),
        Expr::Member { base, .. } => expr(program, handler, base),
        Expr::Construct { fields, .. } => fields
            .iter()
            .for_each(|field| expr(program, handler, field)),
        Expr::Binary { left, right, .. }
        | Expr::With {
            list: left,
            entry: right,
        }
        | Expr::Index {
            base: left,
            index: right,
        } => {
            expr(program, handler, left);
            expr(program, handler, right);
        }
        Expr::Copy { value, .. } => expr(program, handler, value),
        Expr::Call { function, args } => {
            let callee = &program.functions[*function];
            assert_eq!(
                args.len(),
                callee.params.len(),
                "a call passes one argument per param"
            );
            args.iter().for_each(|arg| expr(program, handler, arg));
        }
        Expr::Bytes { op, args } => {
            assert_eq!(args.len(), op.arity(), "a bytes builtin takes its arity");
            args.iter().for_each(|arg| expr(program, handler, arg));
        }
        Expr::Truncate { text, tokens } => {
            assert!(
                ir::truncated_units_max(program.constants[*tokens].value).is_some(),
                "a cut text's bound fits u64"
            );
            expr(program, handler, text);
        }
        Expr::Not(operand) | Expr::Len(operand) | Expr::TextLen(operand) => {
            expr(program, handler, operand);
        }
        Expr::TextBuiltin { op, args } => {
            assert_eq!(args.len(), op.arity(), "a text builtin takes its arity");
            args.iter().for_each(|arg| expr(program, handler, arg));
        }
        Expr::Outcome {
            tag,
            value,
            reason,
            original,
            ..
        } => {
            assert!(
                program.wallet.is_some(),
                "only a harness with a wallet pays"
            );
            for part in [tag, value, reason, original] {
                expr(program, handler, part);
            }
        }
    }
}
