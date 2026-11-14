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
        | ir::Stmt::Take { .. }
        | ir::Stmt::Parallel(_) => true,
        ir::Stmt::If {
            then, otherwise, ..
        } => writes_state(then) || writes_state(otherwise),
        ir::Stmt::Match { arms, .. } => arms.iter().any(|arm| writes_state(&arm.body)),
        ir::Stmt::Loop { body, .. } | ir::Stmt::For { body, .. } => writes_state(body),
        ir::Stmt::Let { .. } | ir::Stmt::Effect { .. } | ir::Stmt::Finish { .. } => false,
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
        } => reads_local(left) || reads_local(right),
        Expr::Not(operand)
        | Expr::Len(operand)
        | Expr::TextLen(operand)
        | Expr::Truncate { text: operand, .. } => reads_local(operand),
        Expr::TextBuiltin { args, .. } => args.iter().any(reads_local),
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
        } => reads_events(left) || reads_events(right),
        Expr::Not(operand)
        | Expr::Len(operand)
        | Expr::TextLen(operand)
        | Expr::Truncate { text: operand, .. } => reads_events(operand),
        Expr::TextBuiltin { args, .. } => args.iter().any(reads_events),
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
            Stmt::Loop { body, bound, .. } | Stmt::For { body, bound, .. } => {
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
                    parallel_pushes_only(&parallel.body),
                    "the body of a parallel for only pushes after its dispatch"
                );
                expr(program, handler, &parallel.list);
                block(
                    program,
                    handler,
                    std::slice::from_ref(&parallel.dispatch()),
                    used,
                );
                block(program, handler, &parallel.body, used);
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
        }
    }
}

fn parallel_pushes_only(statements: &[Stmt]) -> bool {
    statements.iter().all(|statement| match statement {
        Stmt::Push { .. } | Stmt::Let { .. } | Stmt::Assign { .. } => true,
        Stmt::If {
            then, otherwise, ..
        } => parallel_pushes_only(then) && parallel_pushes_only(otherwise),
        Stmt::Match { arms, .. } => arms.iter().all(|arm| parallel_pushes_only(&arm.body)),
        _ => false,
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
        } => {
            expr(program, handler, left);
            expr(program, handler, right);
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
    }
}
