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
pub type CodeId = usize;
pub type ModeId = usize;

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

pub const NOTICE_VARIANT: &str = "notice";
pub const ATTACHED_VARIANT: &str = "attached";
pub const DISPATCHED_TYPE: &str = "dispatched";
pub const DISPATCHED_BLOB: VariantId = 3;
pub const DISPATCHED_VARIANTS: [&str; 4] = ["ok", "failed", "unknown", "blob"];
pub const SEARCH_SEPARATOR: char = '\n';

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blobs {
    pub bytes: Bound,
    pub dispatched: SumId,
    pub attached: VariantId,
}

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
    Blob(Bound),
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
            Self::Blob(bytes) => format!("blob<{}>", bytes.name),
        }
    }

    pub fn holds_blob(&self, program: &Program) -> bool {
        match self {
            Self::Blob(_) => true,
            Self::List { element, .. } => element.holds_blob(program),
            Self::Known(inner) | Self::Maybe(inner) => inner.holds_blob(program),
            Self::Sum(sum) => program.sums[*sum]
                .variants
                .iter()
                .flat_map(|variant| &variant.fields)
                .any(|field| matches!(field, Self::Blob(_))),
            Self::U64 | Self::Bool | Self::Text => false,
        }
    }

    pub fn reports_unknown(&self, program: &Program) -> bool {
        match self {
            Self::Maybe(_) => true,
            Self::Sum(sum) => program
                .blobs
                .as_ref()
                .is_some_and(|blobs| blobs.dispatched == *sum),
            _ => false,
        }
    }

    pub fn variants(&self, program: &Program) -> Option<Vec<Variant>> {
        match self {
            Self::Sum(sum) => Some(program.sums[*sum].variants.clone()),
            Self::Known(inner) => Some(outcome_variants(inner, false)),
            Self::Maybe(inner) => Some(outcome_variants(inner, true)),
            Self::U64 | Self::Bool | Self::Text | Self::List { .. } | Self::Blob(_) => None,
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
    pub codes: Vec<Code>,
    pub listings: Vec<Listing>,
    pub model: Option<Model>,
    pub state: Vec<Field>,
    pub invariants: Vec<Invariant>,
    pub handlers: Vec<Handler>,
    pub sides: Vec<Handler>,
    pub events: Events,
    pub cuts: Vec<u64>,
    pub notice: Option<VariantId>,
    pub blobs: Option<Blobs>,
    pub idle: Option<Idle>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Events {
    pub cancel: bool,
    pub steers: Option<Bound>,
    pub resume: bool,
}

impl Events {
    pub const fn any(&self) -> bool {
        self.cancel || self.steers.is_some()
    }

    pub fn records_max(&self) -> u64 {
        u64::from(self.cancel) + self.steers.as_ref().map_or(0, |bound| bound.value)
    }

    pub fn steers_type(&self) -> Option<Type> {
        self.steers.as_ref().map(|bound| Type::List {
            element: Box::new(Type::Text),
            capacity: bound.clone(),
        })
    }
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

    pub fn side(&self, name: &str) -> Option<&Handler> {
        self.sides.iter().find(|side| side.name == name)
    }

    pub fn kind_of(&self, handler: &Handler) -> &'static str {
        if self.side(&handler.name).is_some() {
            "side handler"
        } else {
            "handler"
        }
    }

    pub fn idles(&self, handler: &str) -> bool {
        self.idle
            .as_ref()
            .is_some_and(|idle| idle.handler == handler)
    }

    pub fn prompted(&self) -> Vec<&Handler> {
        self.handlers
            .iter()
            .filter(|handler| !self.idles(&handler.name))
            .collect()
    }

    pub fn spawn_of(&self, tool: &str) -> Option<&Spawn> {
        self.tool(tool)
            .and_then(|(_, declared)| declared.spawn.as_ref())
    }

    pub fn spawns(&self) -> impl Iterator<Item = (&Tool, &Spawn)> {
        self.tools
            .iter()
            .filter_map(|tool| tool.spawn.as_ref().map(|spawn| (tool, spawn)))
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

    pub fn input_defaults(&self, tool: &str) -> Option<&[Option<Literal>]> {
        let defaults = if let Some((_, declared)) = self.tool(tool) {
            assert_eq!(
                declared.defaults.len(),
                declared.params.len(),
                "a tool has one default slot per parameter"
            );
            &declared.defaults
        } else {
            let (id, _) = self.listed(tool)?;
            let listing = &self.listings[id];
            assert_eq!(
                listing.defaults.len(),
                listing.params.len(),
                "a listing has one default slot per parameter"
            );
            &listing.defaults
        };
        Some(defaults.as_slice())
    }

    pub fn dispatch_result(&self) -> Type {
        match &self.blobs {
            Some(blobs) => Type::Sum(blobs.dispatched),
            None => Type::Maybe(Box::new(Type::Text)),
        }
    }

    pub fn search_of(&self, tool: &str) -> Option<ListingId> {
        self.tool(tool).and_then(|(_, declared)| declared.finds)
    }

    pub fn deferred(&self, listing: ListingId) -> bool {
        self.tools.iter().any(|tool| tool.finds == Some(listing))
    }

    pub fn mode_name(&self, mode: ModeId) -> &str {
        &self
            .model
            .as_ref()
            .expect("a model call carries a mode only with a model")
            .modes[mode]
    }

    pub fn code(&self, name: &str) -> Option<(CodeId, &Code)> {
        self.codes
            .iter()
            .enumerate()
            .find(|(_, code)| code.name == name)
    }

    pub fn resolve_dispatch(&self, tool: &str) -> Resolved {
        match self.code(tool) {
            Some((code, _)) => Resolved::Cell(code),
            None => Resolved::Tool(self.dispatch_class(tool)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolved {
    Tool(EffectClass),
    Cell(CodeId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code {
    pub name: String,
    pub tools: Vec<ToolId>,
    pub calls: Bound,
    pub parallel: Bound,
    pub timers: Bound,
    pub timer_ms: Bound,
    pub journal_bytes: Bound,
    pub yield_ms: Bound,
}

impl Code {
    pub fn worst_class(&self, program: &Program) -> EffectClass {
        self.tools
            .iter()
            .map(|tool| program.tools[*tool].effect)
            .max()
            .expect("the checker gives every code tool a nested tool")
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
    pub defaults: Vec<Option<Literal>>,
    pub returns: Type,
    pub attaches: Option<Bound>,
    pub effect: EffectClass,
    pub spawn: Option<Spawn>,
    pub finds: Option<ListingId>,
}

#[derive(Clone, Debug)]
pub struct Spawn {
    pub file: String,
    pub handler: String,
    pub program: Box<Program>,
    pub sessions: Option<Bound>,
}

impl Spawn {
    pub fn child(&self) -> &Handler {
        self.program
            .handler(&self.handler)
            .expect("the checker spawns only a handler the child harness declares")
    }

    pub fn harness(&self) -> &str {
        &self.program.name
    }

    pub fn tree(&self) -> worst::Tree {
        let worst = &self.child().worst;
        match &worst.family {
            Some(family) => worst::Tree {
                measures: family.total,
                sessions: family.sessions.checked_add(1).expect(
                    "the checker refuses a family whose sessions and the spawn's own pass u64",
                ),
                depth: family.depth,
            },
            None => worst::Tree {
                measures: [
                    worst.effects.total,
                    worst.records.total,
                    worst.requests.total,
                ],
                sessions: 1,
                depth: 0,
            },
        }
    }
}

pub fn spawn_key(turn: u64, seq: u64) -> String {
    format!("{turn}{SPAWN_KEY_SEPARATOR}{seq}")
}

pub fn scoped_spawn_key(scope: &str, turn: u64, seq: u64) -> String {
    if scope.is_empty() {
        return spawn_key(turn, seq);
    }
    format!("{scope}{SPAWN_SCOPE_SEPARATOR}{}", spawn_key(turn, seq))
}

pub const SPAWN_KEY_SEPARATOR: &str = ".";
pub const SPAWN_SCOPE_SEPARATOR: &str = "/";
pub const SPAWN_DEPTH_MAX: u64 = 16;

#[derive(Clone, Debug)]
pub struct Listing {
    pub name: String,
    pub bound: Bound,
    pub params: Vec<Field>,
    pub defaults: Vec<Option<Literal>>,
    pub effect: EffectClass,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Literal {
    U64(u64),
    Bool(bool),
    Text(String),
}

#[derive(Clone, Debug)]
pub struct Model {
    pub name: String,
    pub calls: Bound,
    pub reply: Reply,
    pub window: Option<Window>,
    pub modes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub tokens: Bound,
    pub stop: SumId,
    pub attempt: SumId,
}

pub const STOP_TYPE: &str = "stop";
pub const STOP_VALUES: [&str; 6] = [
    "end",
    "tool_use",
    "max_tokens",
    "refusal",
    "context_exceeded",
    "pause",
];
pub const ATTEMPT_TYPE: &str = "attempt";
pub const ATTEMPT_OK: VariantId = 0;
pub const ATTEMPT_THROTTLED: VariantId = 1;
pub const ATTEMPT_TRANSIENT: VariantId = 2;
pub const ATTEMPT_REJECTED: VariantId = 3;
pub const ATTEMPT_VARIANTS: [&str; 4] = ["ok", "throttled", "transient", "rejected"];
pub const ATTEMPT_REPLY: usize = 0;
pub const ATTEMPT_STOP: usize = 1;
pub const USAGE_FIELDS: [&str; 4] = ["input", "output", "cache_read", "cache_write"];
pub const ATTEMPT_USAGE: usize = 2;
pub const ATTEMPT_OK_FIELDS: usize = ATTEMPT_USAGE + USAGE_FIELDS.len();
pub const ATTEMPT_ERROR: usize = 0;
pub const ATTEMPT_RETRY_AFTER: usize = 1;

pub fn attempt_variants(stop: SumId) -> Vec<Variant> {
    let mut ok = vec![Type::Sum(REPLY), Type::Sum(stop)];
    ok.extend(USAGE_FIELDS.iter().map(|_| Type::U64));
    assert_eq!(
        ok.len(),
        ATTEMPT_OK_FIELDS,
        "ok carries the reply, the stop and the usage"
    );
    let fields = [
        ok,
        vec![Type::Text, Type::U64],
        vec![Type::Text, Type::U64],
        vec![Type::Text],
    ];
    ATTEMPT_VARIANTS
        .iter()
        .zip(fields)
        .map(|(name, fields)| Variant {
            name: (*name).to_owned(),
            fields,
        })
        .collect()
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
        if let Some(window) = &self.window {
            return Type::Sum(window.attempt);
        }
        match self.reply {
            Reply::Builtin => Type::Sum(REPLY),
            Reply::Declared { .. } => Type::Known(Box::new(Type::Sum(REPLY))),
        }
    }

    pub const fn outcome(&self) -> ModelOutcome {
        match (&self.window, self.reply) {
            (Some(_), _) => ModelOutcome::Attempt,
            (None, Reply::Declared { .. }) => ModelOutcome::Known,
            (None, Reply::Builtin) => ModelOutcome::Trap,
        }
    }

    pub const fn declared(&self) -> bool {
        matches!(self.reply, Reply::Declared { .. })
    }
}

pub const MODEL_FAILED: &str = "failed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelOutcome {
    Trap,
    Known,
    Attempt,
}

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
    pub mode: Option<ModeId>,
}

impl Site {
    pub fn traps_on_decode(&self) -> bool {
        self.kind == SiteKind::Model && self.result == Type::Sum(REPLY)
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
    Cell(CodeId),
    Wait(ConstId),
    Suspend,
    Deny,
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
            Self::Cell(_) => "cell",
            Self::Wait(_) => "wait",
            Self::Suspend => "suspend",
            Self::Deny => "deny",
        }
    }

    pub const fn parks(self) -> bool {
        matches!(self, Self::Wait(_) | Self::Suspend)
    }
}

pub const WAIT_TOOL: &str = "wait";
pub const SUSPEND_TOOL: &str = "suspend";
pub const DENY_TOOL: &str = "deny";
pub const SESSION_STARTED_FIELD: &str = "_session_started";
pub const SESSION_ENDED_FIELD: &str = "_session_ended";
pub const HOOK_DECIDED_FIELD: &str = "_hook_decided";
pub const HOOK_DENIED_FIELD: &str = "_hook_denied";
pub const HOOK_REASON_FIELD: &str = "_hook_reason";
pub const HOOK_INPUT_FIELD: &str = "_hook_input";
pub const IDLE_RUNS_FIELD: &str = "_idle_runs";
pub const IDLE_BOUNDED_INVARIANT: &str = "_idle_bounded";
pub const IDLE_AT: &str = "at";
pub const IDLE_DUE: &str = "due";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Idle {
    pub handler: String,
    pub due: FieldId,
    pub bound: Bound,
    pub runs: FieldId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteClass {
    Fixed(EffectClass),
    Dispatched,
    Cell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    RidesNextFlush,
    FlushBeforeRun,
    FlushBeforeRunWhenExternal,
    Cell,
}

impl Durability {
    pub const fn of_site(kind: SiteKind, class: SiteClass) -> Self {
        if kind.parks() {
            return Self::FlushBeforeRun;
        }
        Self::of(class)
    }

    pub const fn of(class: SiteClass) -> Self {
        match class {
            SiteClass::Fixed(EffectClass::External) => Self::FlushBeforeRun,
            SiteClass::Fixed(EffectClass::None | EffectClass::Idempotent) => Self::RidesNextFlush,
            SiteClass::Dispatched => Self::FlushBeforeRunWhenExternal,
            SiteClass::Cell => Self::Cell,
        }
    }

    pub const fn flushes(self, resolved: EffectClass) -> bool {
        match self {
            Self::RidesNextFlush => false,
            Self::FlushBeforeRun => true,
            Self::FlushBeforeRunWhenExternal | Self::Cell => {
                matches!(resolved, EffectClass::External)
            }
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
    KeepBlobs {
        field: FieldId,
        count: Bound,
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
    Take {
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
    LastAfter { count: Expr, first: Expr },
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
        | Stmt::KeepBlobs { .. }
        | Stmt::Effect { .. }
        | Stmt::Take { .. }
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
        | Stmt::KeepBlobs { .. }
        | Stmt::Effect { .. }
        | Stmt::Take { .. }
        | Stmt::Finish { .. } => false,
    })
}

fn expr_cuts(value: &Expr, constants: &[Constant], out: &mut Vec<u64>) {
    match value {
        Expr::Truncate { text, tokens } => {
            out.push(constants[*tokens].value);
            expr_cuts(text, constants, out);
        }
        Expr::Member { base: operand, .. }
        | Expr::Not(operand)
        | Expr::Len(operand)
        | Expr::TextLen(operand) => expr_cuts(operand, constants, out),
        Expr::Construct { fields, .. } | Expr::TextBuiltin { args: fields, .. } => fields
            .iter()
            .for_each(|field| expr_cuts(field, constants, out)),
        Expr::Binary { left, right, .. }
        | Expr::With {
            list: left,
            entry: right,
        } => {
            expr_cuts(left, constants, out);
            expr_cuts(right, constants, out);
        }
        Expr::U64(_)
        | Expr::Bool(_)
        | Expr::Text(_)
        | Expr::Constant(_)
        | Expr::Local(_)
        | Expr::Field(_)
        | Expr::ListingNotice(_)
        | Expr::Cancelled => {}
    }
}

fn block_cuts(statements: &[Stmt], constants: &[Constant], out: &mut Vec<u64>) {
    for statement in statements {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Push { value, .. }
            | Stmt::Finish { value } => expr_cuts(value, constants, out),
            Stmt::Shrink { kept, .. } => match kept {
                Kept::Nothing => {}
                Kept::Last(count) => expr_cuts(count, constants, out),
                Kept::LastAfter { count, first } => {
                    expr_cuts(count, constants, out);
                    expr_cuts(first, constants, out);
                }
            },
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                expr_cuts(condition, constants, out);
                block_cuts(then, constants, out);
                block_cuts(otherwise, constants, out);
            }
            Stmt::Match { subject, arms, .. } => {
                expr_cuts(subject, constants, out);
                arms.iter()
                    .for_each(|arm| block_cuts(&arm.body, constants, out));
            }
            Stmt::Loop { body, .. } => block_cuts(body, constants, out),
            Stmt::For { list, body, .. } => {
                expr_cuts(list, constants, out);
                block_cuts(body, constants, out);
            }
            Stmt::Parallel(parallel) => {
                expr_cuts(&parallel.list, constants, out);
                parallel
                    .args
                    .iter()
                    .for_each(|arg| expr_cuts(arg, constants, out));
                block_cuts(&parallel.body, constants, out);
            }
            Stmt::Effect { args, .. } => args.iter().for_each(|arg| expr_cuts(arg, constants, out)),
            Stmt::Take { .. } | Stmt::KeepBlobs { .. } => {}
        }
    }
}

impl Program {
    pub fn cut_tokens(&self) -> &[u64] {
        &self.cuts
    }

    pub fn find_cuts(&self) -> Vec<u64> {
        let mut cuts = Vec::new();
        for handler in self.handlers.iter().chain(&self.sides) {
            block_cuts(&handler.body, &self.constants, &mut cuts);
        }
        cuts.sort_unstable();
        cuts.dedup();
        cuts
    }

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
    TextLen(Box<Expr>),
    Truncate {
        text: Box<Expr>,
        tokens: ConstId,
    },
    With {
        list: Box<Expr>,
        entry: Box<Expr>,
    },
    TextBuiltin {
        op: TextOp,
        args: Vec<Expr>,
    },
    ListingNotice(ListingId),
    Cancelled,
}

pub const UTF16_UNITS_PER_TOKEN: u64 = 4;
pub const TRUNCATION_OPEN: &str = "\u{2026}";
pub const TRUNCATION_CLOSE: &str = " tokens truncated\u{2026}";
pub const U64_DECIMAL_DIGITS_MAX: u64 = 20;

pub fn estimated_tokens(text: &str) -> u64 {
    (text.encode_utf16().count() as u64).div_ceil(UTF16_UNITS_PER_TOKEN)
}

pub fn utf16_len(text: &str) -> u64 {
    text.encode_utf16().count() as u64
}

pub fn truncation_marker_units_max() -> u64 {
    utf16_len(TRUNCATION_OPEN) + U64_DECIMAL_DIGITS_MAX + utf16_len(TRUNCATION_CLOSE)
}

pub fn truncated_units_max(tokens: u64) -> Option<u64> {
    tokens
        .checked_mul(UTF16_UNITS_PER_TOKEN)?
        .checked_add(truncation_marker_units_max())
}

pub fn truncate_middle(text: &str, tokens_max: u64) -> String {
    let total = estimated_tokens(text);
    if total <= tokens_max {
        return text.to_owned();
    }
    let side = tokens_max.saturating_mul(UTF16_UNITS_PER_TOKEN / 2);
    let mut units = 0_u64;
    let head = text
        .char_indices()
        .find(|(_, character)| {
            units += character.len_utf16() as u64;
            units > side
        })
        .map_or(text.len(), |(end, _)| end);
    units = 0;
    let tail = text
        .char_indices()
        .rev()
        .find(|(_, character)| {
            units += character.len_utf16() as u64;
            units > side
        })
        .map_or(0, |(start, character)| start + character.len_utf8())
        .max(head);
    let cut = format!(
        "{}{TRUNCATION_OPEN}{}{TRUNCATION_CLOSE}{}",
        &text[..head],
        total - tokens_max,
        &text[tail..]
    );
    assert!(
        truncated_units_max(tokens_max).is_none_or(|max| utf16_len(&cut) <= max),
        "a cut text fits its declared bound"
    );
    cut
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextOp {
    Contains,
    Before,
    After,
    Trim,
}

pub const TEXT_WHITESPACE: [char; 5] = [' ', '\t', '\n', '\x0c', '\r'];

impl TextOp {
    pub const ALL: [Self; 4] = [Self::Contains, Self::Before, Self::After, Self::Trim];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Before => "before",
            Self::After => "after",
            Self::Trim => "trim",
        }
    }

    pub const fn arity(self) -> usize {
        match self {
            Self::Contains | Self::Before | Self::After => 2,
            Self::Trim => 1,
        }
    }

    pub fn result(self) -> Type {
        match self {
            Self::Contains => Type::Bool,
            Self::Before | Self::After | Self::Trim => Type::Text,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.name() == name)
    }
}

pub enum TextValue {
    Bool(bool),
    Text(String),
}

pub fn text_op(op: TextOp, subject: &str, separator: Option<&str>) -> TextValue {
    match (op, separator) {
        (TextOp::Contains, Some(separator)) => TextValue::Bool(subject.contains(separator)),
        (TextOp::Before, Some(separator)) => TextValue::Text(
            subject
                .split_once(separator)
                .map_or(subject, |(before, _)| before)
                .to_owned(),
        ),
        (TextOp::After, Some(separator)) => TextValue::Text(
            subject
                .split_once(separator)
                .map_or("", |(_, after)| after)
                .to_owned(),
        ),
        (TextOp::Trim, None) => {
            TextValue::Text(subject.trim_matches(TEXT_WHITESPACE.as_slice()).to_owned())
        }
        (op, separator) => panic!(
            "text builtin {} takes {} argument(s), given a separator: {}",
            op.name(),
            op.arity(),
            separator.is_some()
        ),
    }
}
