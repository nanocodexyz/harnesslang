pub mod validate;
pub mod worst;

use crate::effect::EffectClass;
pub use crate::syntax::ast::BinaryOp;

pub type ConstId = usize;
pub type SumId = usize;
pub type ToolId = usize;
pub type ListingId = usize;
pub type FieldId = usize;
pub type LocalId = usize;
pub type SiteId = usize;
pub type VariantId = usize;

pub const CALL: SumId = 0;
pub const REPLY: SumId = 1;
pub const ENTRY: SumId = 2;
pub const BUILTIN_SUMS: usize = 3;

pub const CALL_ID: usize = 0;
pub const CALL_TOOL: usize = 1;
pub const CALL_INPUT: usize = 2;
pub const CALL_FIELDS: [&str; 3] = ["id", "tool", "input"];

pub const ANSWER: VariantId = 0;
pub const CALLS: VariantId = 1;

pub const ENTRY_USER: VariantId = 0;
pub const ENTRY_ASSISTANT: VariantId = 1;
pub const ENTRY_RESULT: VariantId = 2;
pub const ENTRY_ERROR: VariantId = 3;
pub const ENTRY_UNKNOWN: VariantId = 4;

pub const OK: VariantId = 0;
pub const FAILED: VariantId = 1;
pub const UNKNOWN: VariantId = 2;
pub const OUTCOME_VARIANTS: [&str; 3] = ["ok", "failed", "unknown"];

pub const LISTED_TOOL_SEPARATOR: &str = "__";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    pub name: String,
    pub value: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    U64,
    Bool,
    Text,
    Sum(SumId),
    List { element: Box<Type>, capacity: Bound },
    Known(Box<Type>),
    Maybe(Box<Type>),
}

impl Type {
    pub fn name(&self, program: &Program) -> String {
        match self {
            Self::U64 => "u64".to_owned(),
            Self::Bool => "bool".to_owned(),
            Self::Text => "text".to_owned(),
            Self::Sum(sum) => program.sums[*sum].name.clone(),
            Self::List { element, capacity } => {
                format!("list<{}, {}>", element.name(program), capacity.name)
            }
            Self::Known(inner) => format!("known<{}>", inner.name(program)),
            Self::Maybe(inner) => format!("maybe<{}>", inner.name(program)),
        }
    }

    pub fn variants(&self, program: &Program) -> Option<Vec<Variant>> {
        match self {
            Self::Sum(sum) => Some(program.sums[*sum].variants.clone()),
            Self::Known(inner) => Some(outcome_variants(inner, false)),
            Self::Maybe(inner) => Some(outcome_variants(inner, true)),
            Self::U64 | Self::Bool | Self::Text | Self::List { .. } => None,
        }
    }
}

fn outcome_variants(inner: &Type, unknown: bool) -> Vec<Variant> {
    let mut variants = vec![
        Variant {
            name: OUTCOME_VARIANTS[OK].to_owned(),
            fields: vec![inner.clone()],
        },
        Variant {
            name: OUTCOME_VARIANTS[FAILED].to_owned(),
            fields: vec![Type::Text],
        },
    ];
    if unknown {
        variants.push(Variant {
            name: OUTCOME_VARIANTS[UNKNOWN].to_owned(),
            fields: Vec::new(),
        });
    }
    variants
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sum {
    pub name: String,
    pub kind: SumKind,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SumKind {
    Tagged,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Type>,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub name: String,
    pub constants: Vec<Constant>,
    pub sums: Vec<Sum>,
    pub tools: Vec<Tool>,
    pub listings: Vec<Listing>,
    pub model: Option<Model>,
    pub state: Vec<Field>,
    pub invariants: Vec<Invariant>,
    pub handlers: Vec<Handler>,
}

#[derive(Clone, Debug)]
pub struct Invariant {
    pub name: String,
    pub condition: Expr,
}

impl Program {
    pub fn tool(&self, name: &str) -> Option<(ToolId, &Tool)> {
        self.tools
            .iter()
            .enumerate()
            .find(|(_, tool)| tool.name == name)
    }

    pub fn handler(&self, name: &str) -> Option<&Handler> {
        self.handlers.iter().find(|handler| handler.name == name)
    }

    pub fn text_enum(&self, ty: &Type) -> Option<&Sum> {
        match ty {
            Type::Sum(sum) if self.sums[*sum].kind == SumKind::Text => {
                let found = &self.sums[*sum];
                assert!(
                    found
                        .variants
                        .iter()
                        .all(|variant| variant.fields.is_empty()),
                    "a text enum's values carry no fields"
                );
                Some(found)
            }
            _ => None,
        }
    }

    pub fn listed<'t>(&self, tool: &'t str) -> Option<(ListingId, &'t str)> {
        let (prefix, name) = tool.split_once(LISTED_TOOL_SEPARATOR)?;
        if name.is_empty() {
            return None;
        }
        let id = self
            .listings
            .iter()
            .position(|listing| listing.name == prefix)?;
        Some((id, name))
    }

    pub fn dispatch_class(&self, tool: &str) -> EffectClass {
        if let Some((_, declared)) = self.tool(tool) {
            return declared.effect;
        }
        self.listed(tool)
            .map_or(EffectClass::undeclared(), |(id, _)| {
                self.listings[id].effect
            })
    }

    pub fn input_params(&self, tool: &str) -> Option<&[Field]> {
        if let Some((_, declared)) = self.tool(tool) {
            return Some(&declared.params);
        }
        self.listed(tool)
            .map(|(id, _)| self.listings[id].params.as_slice())
    }
}

#[derive(Clone, Debug)]
pub struct Constant {
    pub name: String,
    pub value: u64,
}

#[derive(Clone, Debug)]
pub struct Tool {
    pub name: String,
    pub params: Vec<Field>,
    pub returns: Type,
    pub effect: EffectClass,
}

#[derive(Clone, Debug)]
pub struct Listing {
    pub name: String,
    pub bound: Bound,
    pub params: Vec<Field>,
    pub effect: EffectClass,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub name: String,
    pub calls: Bound,
    pub reply: Reply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Builtin,
    Declared { calls: VariantId, field: usize },
}

impl Model {
    pub const fn calls_at(&self) -> (VariantId, usize) {
        match self.reply {
            Reply::Builtin => (CALLS, 0),
            Reply::Declared { calls, field } => (calls, field),
        }
    }

    pub fn result(&self) -> Type {
        match self.reply {
            Reply::Builtin => Type::Sum(REPLY),
            Reply::Declared { .. } => Type::Known(Box::new(Type::Sum(REPLY))),
        }
    }

    pub const fn declared(&self) -> bool {
        matches!(self.reply, Reply::Declared { .. })
    }
}

pub const MODEL_FAILED: &str = "failed";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
}

#[derive(Clone, Debug)]
pub struct Handler {
    pub name: String,
    pub params: Vec<LocalId>,
    pub returns: Type,
    pub requires: Option<Expr>,
    pub locals: Vec<Field>,
    pub sites: Vec<Site>,
    pub body: Vec<Stmt>,
    pub worst: worst::WorstCase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub kind: SiteKind,
    pub class: SiteClass,
    pub result: Type,
    pub durability: Durability,
}

impl Site {
    pub fn traps_on_decode(&self) -> bool {
        self.kind == SiteKind::Model && !matches!(self.result, Type::Known(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteKind {
    Tool(ToolId),
    Listing(ListingId),
    Dispatch,
    Model,
    Now,
    Random,
}

impl SiteKind {
    pub const fn outbound(self) -> bool {
        matches!(
            self,
            Self::Tool(_) | Self::Listing(_) | Self::Dispatch | Self::Model
        )
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Tool(_) => "tool",
            Self::Listing(_) => "listing",
            Self::Dispatch => "dispatch",
            Self::Model => "model",
            Self::Now => "now",
            Self::Random => "random",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteClass {
    Fixed(EffectClass),
    Dispatched,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    RidesNextFlush,
    FlushBeforeRun,
    FlushBeforeRunWhenExternal,
}

impl Durability {
    pub const fn of(class: SiteClass) -> Self {
        match class {
            SiteClass::Fixed(EffectClass::External) => Self::FlushBeforeRun,
            SiteClass::Fixed(EffectClass::None | EffectClass::Idempotent) => Self::RidesNextFlush,
            SiteClass::Dispatched => Self::FlushBeforeRunWhenExternal,
        }
    }

    pub const fn flushes(self, resolved: EffectClass) -> bool {
        match self {
            Self::RidesNextFlush => false,
            Self::FlushBeforeRun => true,
            Self::FlushBeforeRunWhenExternal => matches!(resolved, EffectClass::External),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let {
        local: LocalId,
        value: Expr,
    },
    Assign {
        field: FieldId,
        value: Expr,
    },
    Push {
        field: FieldId,
        value: Expr,
    },
    Shrink {
        field: FieldId,
        kept: Kept,
    },
    If {
        condition: Expr,
        then: Vec<Stmt>,
        otherwise: Vec<Stmt>,
    },
    Match {
        subject: Expr,
        ty: Type,
        arms: Vec<Arm>,
    },
    Loop {
        index: LocalId,
        bound: Bound,
        body: Vec<Stmt>,
    },
    For {
        item: LocalId,
        list: Expr,
        bound: Bound,
        body: Vec<Stmt>,
    },
    Parallel(Parallel),
    Effect {
        site: SiteId,
        args: Vec<Expr>,
        result: LocalId,
    },
    Finish {
        value: Expr,
    },
}

#[derive(Clone, Debug)]
pub enum Kept {
    Nothing,
    Last(Expr),
}

#[derive(Clone, Debug)]
pub struct Parallel {
    pub item: LocalId,
    pub list: Expr,
    pub bound: Bound,
    pub site: SiteId,
    pub args: Vec<Expr>,
    pub result: LocalId,
    pub body: Vec<Stmt>,
}

impl Parallel {
    pub fn dispatch(&self) -> Stmt {
        Stmt::Effect {
            site: self.site,
            args: self.args.clone(),
            result: self.result,
        }
    }

    pub fn sequential(&self) -> Stmt {
        let mut body = Vec::with_capacity(self.body.len() + 1);
        body.push(self.dispatch());
        body.extend(sequential_block(&self.body));
        Stmt::For {
            item: self.item,
            list: self.list.clone(),
            bound: self.bound.clone(),
            body,
        }
    }
}

pub fn sequential_block(statements: &[Stmt]) -> Vec<Stmt> {
    statements.iter().map(sequential_statement).collect()
}

fn sequential_statement(statement: &Stmt) -> Stmt {
    match statement {
        Stmt::Parallel(parallel) => parallel.sequential(),
        Stmt::If {
            condition,
            then,
            otherwise,
        } => Stmt::If {
            condition: condition.clone(),
            then: sequential_block(then),
            otherwise: sequential_block(otherwise),
        },
        Stmt::Match { subject, ty, arms } => Stmt::Match {
            subject: subject.clone(),
            ty: ty.clone(),
            arms: arms
                .iter()
                .map(|arm| Arm {
                    variant: arm.variant,
                    bindings: arm.bindings.clone(),
                    body: sequential_block(&arm.body),
                })
                .collect(),
        },
        Stmt::Loop { index, bound, body } => Stmt::Loop {
            index: *index,
            bound: bound.clone(),
            body: sequential_block(body),
        },
        Stmt::For {
            item,
            list,
            bound,
            body,
        } => Stmt::For {
            item: *item,
            list: list.clone(),
            bound: bound.clone(),
            body: sequential_block(body),
        },
        Stmt::Let { .. }
        | Stmt::Assign { .. }
        | Stmt::Push { .. }
        | Stmt::Shrink { .. }
        | Stmt::Effect { .. }
        | Stmt::Finish { .. } => statement.clone(),
    }
}

pub fn has_parallel(statements: &[Stmt]) -> bool {
    statements.iter().any(|statement| match statement {
        Stmt::Parallel(_) => true,
        Stmt::If {
            then, otherwise, ..
        } => has_parallel(then) || has_parallel(otherwise),
        Stmt::Match { arms, .. } => arms.iter().any(|arm| has_parallel(&arm.body)),
        Stmt::Loop { body, .. } | Stmt::For { body, .. } => has_parallel(body),
        Stmt::Let { .. }
        | Stmt::Assign { .. }
        | Stmt::Push { .. }
        | Stmt::Shrink { .. }
        | Stmt::Effect { .. }
        | Stmt::Finish { .. } => false,
    })
}

impl Program {
    pub fn has_parallel(&self) -> bool {
        self.handlers
            .iter()
            .any(|handler| has_parallel(&handler.body))
    }

    pub fn sequential(&self) -> Self {
        let mut program = self.clone();
        for handler in &mut program.handlers {
            handler.body = sequential_block(&handler.body);
        }
        program
    }
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub variant: VariantId,
    pub bindings: Vec<Option<LocalId>>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub enum Expr {
    U64(u64),
    Bool(bool),
    Text(String),
    Constant(ConstId),
    Local(LocalId),
    Field(FieldId),
    Member {
        base: Box<Expr>,
        index: usize,
    },
    Construct {
        ty: Type,
        variant: VariantId,
        fields: Vec<Expr>,
    },
    Binary {
        op: BinaryOp,
        operand: Type,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Not(Box<Expr>),
    Len(Box<Expr>),
}
