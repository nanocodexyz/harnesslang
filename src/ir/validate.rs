use crate::ir::{
    self, Durability, Expr, Handler, Program, SiteClass, SiteKind, Stmt, SumKind, Type,
};

pub fn program(program: &Program) {
    assert!(
        program.sums.len() >= crate::ir::BUILTIN_SUMS,
        "every program carries the builtin sums"
    );
    for invariant in &program.invariants {
        assert!(
            !reads_local(&invariant.condition),
            "an invariant reads only state and constants"
        );
    }
    reply(program);
    for handler in &program.handlers {
        self::handler(program, handler);
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
}

fn reads_local(value: &Expr) -> bool {
    match value {
        Expr::U64(_) | Expr::Bool(_) | Expr::Text(_) | Expr::Constant(_) | Expr::Field(_) => false,
        Expr::Local(_) => true,
        Expr::Member { base, .. } => reads_local(base),
        Expr::Construct { fields, .. } => fields.iter().any(reads_local),
        Expr::Binary { left, right, .. } => reads_local(left) || reads_local(right),
        Expr::Not(operand) | Expr::Len(operand) => reads_local(operand),
    }
}

fn handler(program: &Program, handler: &Handler) {
    let mut used = vec![0_u32; handler.sites.len()];
    for site in &handler.sites {
        assert_eq!(
            site.durability,
            Durability::of(site.class),
            "a site's durability follows from its class"
        );
        if let SiteKind::Tool(tool) = site.kind {
            assert_eq!(
                site.class,
                SiteClass::Fixed(program.tools[tool].effect),
                "a tool site has its tool's class"
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
                Type::Maybe(Box::new(Type::Text)),
                "dispatch yields maybe<text>"
            );
        }
    }
    for param in &handler.params {
        assert!(*param < handler.locals.len(), "a parameter is a local");
    }
    if let Some(requires) = &handler.requires {
        expr(handler, requires);
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
                expr(handler, value);
            }
            Stmt::Assign { field, value } | Stmt::Push { field, value } => {
                assert!(
                    *field < program.state.len(),
                    "assignments target state fields"
                );
                expr(handler, value);
            }
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                expr(handler, condition);
                block(program, handler, then, used);
                block(program, handler, otherwise, used);
            }
            Stmt::Match { subject, ty, arms } => {
                expr(handler, subject);
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
            Stmt::Effect { site, args, result } => {
                assert!(*result < handler.locals.len(), "an effect binds a local");
                assert_eq!(
                    handler.locals[*result].ty, handler.sites[*site].result,
                    "an effect's local has its site's result type"
                );
                used[*site] += 1;
                args.iter().for_each(|arg| expr(handler, arg));
            }
            Stmt::Finish { value } => expr(handler, value),
        }
    }
}

fn expr(handler: &Handler, value: &Expr) {
    match value {
        Expr::U64(_) | Expr::Bool(_) | Expr::Text(_) | Expr::Constant(_) | Expr::Field(_) => {}
        Expr::Local(local) => assert!(*local < handler.locals.len(), "a local exists"),
        Expr::Member { base, .. } => expr(handler, base),
        Expr::Construct { fields, .. } => fields.iter().for_each(|field| expr(handler, field)),
        Expr::Binary { left, right, .. } => {
            expr(handler, left);
            expr(handler, right);
        }
        Expr::Not(operand) | Expr::Len(operand) => expr(handler, operand),
    }
}
