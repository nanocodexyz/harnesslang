use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{self, Expr, Handler, LocalId, Program, SiteId, SiteKind, Stmt};

pub struct Names<'a> {
    program: &'a Program,
    handler: Option<&'a Handler>,
    effects: BTreeMap<LocalId, (SiteId, &'a [Expr])>,
    takes: BTreeSet<LocalId>,
}

impl<'a> Names<'a> {
    pub fn new(program: &'a Program, handler: Option<&'a Handler>) -> Self {
        let mut effects = BTreeMap::new();
        let mut takes = BTreeSet::new();
        if let Some(handler) = handler {
            collect(&handler.body, &mut effects, &mut takes);
        }
        Self {
            program,
            handler,
            effects,
            takes,
        }
    }

    pub fn site(&self, site: SiteId, args: &[Expr]) -> String {
        let handler = self.handler.expect("a site belongs to a handler");
        let name = match handler.sites[site].kind {
            SiteKind::Tool(tool) => self.program.tools[tool].name.clone(),
            SiteKind::Listing(listing) => self.program.listings[listing].name.clone(),
            SiteKind::Model => self
                .program
                .model
                .as_ref()
                .map_or_else(|| "model".to_owned(), |model| model.name.clone()),
            kind => kind.name().to_owned(),
        };
        let args: Vec<String> = args.iter().map(|arg| self.expr(arg)).collect();
        format!("{name}({})", args.join(", "))
    }

    pub fn local(&self, local: LocalId) -> String {
        if let Some((site, args)) = self.effects.get(&local) {
            return self.site(*site, args);
        }
        if self.takes.contains(&local) {
            return format!("take {}", crate::check::STEERS_READ);
        }
        let handler = self.handler.expect("a local belongs to a handler");
        handler.locals[local].name.clone()
    }

    pub fn expr(&self, value: &Expr) -> String {
        match value {
            Expr::U64(number) => number.to_string(),
            Expr::Bool(flag) => flag.to_string(),
            Expr::Text(text) => format!("{text:?}"),
            Expr::Constant(constant) => self.program.constants[*constant].name.clone(),
            Expr::Local(local) => self.local(*local),
            Expr::Field(field) => self.program.state[*field].name.clone(),
            Expr::Member { base, index } => {
                format!("{}.{}", self.expr(base), ir::CALL_FIELDS[*index])
            }
            Expr::Construct {
                ty,
                variant,
                fields,
            } => {
                let variants = ty.variants(self.program).expect("a construct builds a sum");
                let name = &variants[*variant].name;
                if fields.is_empty() {
                    return name.clone();
                }
                let fields: Vec<String> = fields.iter().map(|field| self.expr(field)).collect();
                format!("{name}({})", fields.join(", "))
            }
            Expr::Binary {
                op, left, right, ..
            } => format!(
                "{} {} {}",
                self.operand(left),
                op.symbol(),
                self.operand(right)
            ),
            Expr::Not(operand) => format!("not {}", self.operand(operand)),
            Expr::Len(operand) => format!("len({})", self.expr(operand)),
            Expr::Cancelled => crate::check::CANCELLED_READ.to_owned(),
        }
    }

    fn operand(&self, value: &Expr) -> String {
        match value {
            Expr::Binary { .. } => format!("({})", self.expr(value)),
            _ => self.expr(value),
        }
    }
}

fn collect<'a>(
    statements: &'a [Stmt],
    effects: &mut BTreeMap<LocalId, (SiteId, &'a [Expr])>,
    takes: &mut BTreeSet<LocalId>,
) {
    for statement in statements {
        match statement {
            Stmt::Effect { site, args, result } => {
                effects.insert(*result, (*site, args.as_slice()));
            }
            Stmt::Take { result } => {
                takes.insert(*result);
            }
            Stmt::If {
                then, otherwise, ..
            } => {
                collect(then, effects, takes);
                collect(otherwise, effects, takes);
            }
            Stmt::Match { arms, .. } => arms
                .iter()
                .for_each(|arm| collect(&arm.body, effects, takes)),
            Stmt::Loop { body, .. } | Stmt::For { body, .. } => collect(body, effects, takes),
            Stmt::Parallel(parallel) => {
                effects.insert(parallel.result, (parallel.site, parallel.args.as_slice()));
                collect(&parallel.body, effects, takes);
            }
            Stmt::Let { .. } | Stmt::Assign { .. } | Stmt::Push { .. } | Stmt::Finish { .. } => {}
        }
    }
}
