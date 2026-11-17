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
    Bytes(Bound),
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
            Self::Bytes(capacity) => format!("bytes<{}>", capacity.name),
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
            Self::U64 | Self::Bool | Self::Text | Self::Bytes(_) => false,
        }
    }

    pub fn holds_bytes(&self, program: &Program) -> bool {
        match self {
            Self::Bytes(_) => true,
            Self::List { element, .. } => element.holds_bytes(program),
            Self::Known(inner) | Self::Maybe(inner) => inner.holds_bytes(program),
            Self::Sum(sum) => program.sums[*sum]
                .variants
                .iter()
                .flat_map(|variant| &variant.fields)
                .any(|field| field.holds_bytes(program)),
            Self::U64 | Self::Bool | Self::Text | Self::Blob(_) => false,
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
            Self::U64
            | Self::Bool
            | Self::Text
            | Self::List { .. }
            | Self::Blob(_)
            | Self::Bytes(_) => None,
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
    pub functions: Vec<Handler>,
    pub events: Events,
    pub cuts: Vec<u64>,
    pub notice: Option<VariantId>,
    pub blobs: Option<Blobs>,
    pub idle: Option<Idle>,
    pub forked: Option<Handler>,
    pub wallet: Option<Wallet>,
    pub climbs: Vec<Climb>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wallet {
    pub name: String,
    pub spend: Bound,
    pub charge: Bound,
    pub payments: Bound,
    pub tool: ToolId,
    pub spent: FieldId,
}

pub const PAYMENT_REQUIRED_PREFIX: &str = "payment required: ";
pub const PAYMENT_PARAM: &str = "payment";
pub const WALLET_REQUIRED_PARAM: &str = "required";
pub const WALLET_MAX_PARAM: &str = "max";
pub const SPENT_FIELD: &str = "_spent";
pub const PAID_TAG_FIELD: &str = "_paid_tag";
pub const PAID_MORE_FIELD: &str = "_paid_more";
pub const PAID_REF_FIELD: &str = "_paid_ref";
pub const PAID_TEXT_FIELD: &str = "_paid_text";
pub const PAID_VALUE_FIELD: &str = "_paid_value_";
pub const SPEND_INVARIANT: &str = "_spend_within_cap";
pub const SPENT_BUILTIN: &str = "spent";
pub const PAYMENT_OVERCHARGED: &str = "the wallet charged more than the payment's max";
pub const OUTCOME_TAG_UNKNOWN: u64 = 2;

pub fn wallet_refusal(wallet: &str) -> String {
    format!(
        "input of {wallet}: {wallet} is the harness's wallet; only a paid call's payment reaches it"
    )
}

pub fn with_payment(input: &str, payment: &str) -> String {
    match crate::run::json::decode(input) {
        Ok(crate::run::json::Json::Object(mut fields)) => {
            fields.insert(
                PAYMENT_PARAM.to_owned(),
                crate::run::json::Json::String(payment.to_owned()),
            );
            crate::run::json::Json::Object(fields).encode()
        }
        _ => input.to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Climb {
    pub name: String,
    pub over: FieldId,
    pub ledger: FieldId,
    pub evaluated: FieldId,
    pub cycles: u64,
    pub candidates: u64,
    pub trials: u64,
    pub budget: Option<u64>,
    pub higher: bool,
    pub rule: ClimbRule,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClimbRule {
    Mean(u64),
    Sign(u64),
    Best(u64),
    When,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Events {
    pub cancel: bool,
    pub steers: Option<Bound>,
    pub jobs: Option<Bound>,
    pub resume: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    Steers,
    Jobs,
}

impl Taken {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Steers => "steer",
            Self::Jobs => "job",
        }
    }
}

impl Events {
    pub const fn any(&self) -> bool {
        self.cancel || self.steers.is_some() || self.jobs.is_some()
    }

    pub fn records_max(&self) -> u64 {
        u64::from(self.cancel)
            + self.steers.as_ref().map_or(0, |bound| bound.value)
            + self.jobs.as_ref().map_or(0, |bound| bound.value)
    }

    pub const fn taken(&self, taken: Taken) -> Option<&Bound> {
        match taken {
            Taken::Steers => self.steers.as_ref(),
            Taken::Jobs => self.jobs.as_ref(),
        }
    }

    pub fn taken_type(&self, taken: Taken) -> Option<Type> {
        self.taken(taken).map(|bound| Type::List {
            element: Box::new(Type::Text),
            capacity: bound.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interrupts {
    pub cancel: bool,
    pub steer: bool,
}

impl Interrupts {
    pub const fn any(self) -> bool {
        self.cancel || self.steer
    }

    pub fn describe(self) -> String {
        let mut names = Vec::new();
        if self.cancel {
            names.push("cancel");
        }
        if self.steer {
            names.push("steer");
        }
        names.join(", ")
    }
}

pub const INTERRUPTED_REASON: &str = "Tool execution interrupted; outcome unknown. Do not assume it did not run or automatically repeat it.";
pub const JOB_KEY: &str = "job";

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

    pub fn interrupts(&self, kind: SiteKind, tool: &str) -> Interrupts {
        match kind {
            SiteKind::Model => self
                .model
                .as_ref()
                .map_or_else(Interrupts::default, |model| model.interrupts),
            SiteKind::Tool(_) | SiteKind::Dispatch | SiteKind::Cell(_) => {
                if let Some(code) = self.cell_of(tool) {
                    return self.codes[code].interrupts;
                }
                self.tool(tool)
                    .map_or_else(Interrupts::default, |(_, declared)| declared.interrupts)
            }
            _ => Interrupts::default(),
        }
    }

    pub fn paid(&self, tool: &str) -> bool {
        if let Some((_, declared)) = self.tool(tool) {
            return declared.paid;
        }
        self.listed(tool)
            .is_some_and(|(id, _)| self.listings[id].paid)
    }

    pub fn is_wallet(&self, tool: &str) -> bool {
        self.wallet
            .as_ref()
            .is_some_and(|wallet| wallet.name == tool)
    }

    pub fn recoverable(&self, tool: &str) -> bool {
        self.tool(tool)
            .is_some_and(|(_, declared)| declared.recoverable)
    }

    pub fn jobs_of(&self, tool: &str) -> Option<&Bound> {
        self.tool(tool)
            .and_then(|(_, declared)| declared.jobs.as_ref())
    }

    pub fn any_interruptible(&self) -> bool {
        self.model
            .as_ref()
            .is_some_and(|model| model.interrupts.any())
            || self.tools.iter().any(|tool| tool.interrupts.any())
            || self.codes.iter().any(|code| code.interrupts.any())
    }

    pub fn cell_of(&self, tool: &str) -> Option<CodeId> {
        self.code(tool)
            .map(|(code, _)| code)
            .or_else(|| self.waits(tool).map(|(code, _)| code))
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
        } else if self
            .forked
            .as_ref()
            .is_some_and(|forked| std::ptr::eq(forked, handler))
        {
            "hook"
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

    pub fn fork_of(&self, tool: &str) -> Option<ForkAt> {
        self.tool(tool).and_then(|(_, declared)| declared.fork)
    }

    pub fn forks(&self) -> bool {
        self.tools.iter().any(|tool| tool.fork.is_some())
    }

    pub fn starts_sessions(&self, tool: &Tool) -> bool {
        tool.spawn.is_some() || tool.fork.is_some()
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
        if self.waits(tool).is_some() {
            return EffectClass::None;
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
        if let Some((_, waits)) = self.waits(tool) {
            return Some(&waits.params);
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
        } else if let Some((_, waits)) = self.waits(tool) {
            &waits.defaults
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

    pub fn cells_yield(&self) -> bool {
        self.codes.iter().any(|code| code.waits.is_some())
    }

    pub fn waits(&self, tool: &str) -> Option<(CodeId, &Waits)> {
        self.codes.iter().enumerate().find_map(|(code, declared)| {
            declared
                .waits
                .as_ref()
                .filter(|waits| waits.name == tool)
                .map(|waits| (code, waits))
        })
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
    pub waits: Option<Waits>,
    pub interrupts: Interrupts,
}

pub const WAIT_CELL_ID: &str = "cell_id";
pub const WAIT_TERMINATE: &str = "terminate";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waits {
    pub name: String,
    pub params: Vec<Field>,
    pub defaults: Vec<Option<Literal>>,
}

impl Waits {
    pub fn new(name: String) -> Self {
        Self {
            name,
            params: vec![
                Field {
                    name: WAIT_CELL_ID.to_owned(),
                    ty: Type::Text,
                },
                Field {
                    name: WAIT_TERMINATE.to_owned(),
                    ty: Type::Bool,
                },
            ],
            defaults: vec![None, Some(Literal::Bool(false))],
        }
    }
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
    pub recoverable: bool,
    pub interrupts: Interrupts,
    pub jobs: Option<Bound>,
    pub spawn: Option<Spawn>,
    pub fork: Option<ForkAt>,
    pub finds: Option<ListingId>,
    pub paid: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkAt {
    Latest,
    Turn,
}

impl ForkAt {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Turn => "turn",
        }
    }
}

pub const FORKED_HOST_KEY: &str = "forked";
pub const FORKED_HOOK: &str = "forked";
pub const FORK_TURN: &str = "turn";
pub const FORK_ORIGIN: &str = "origin";
pub const FORK_DISCARDED: &str = "discarded";
pub const FORK_STATE: &str = "state";

pub fn fork_missing(turn: u64) -> String {
    format!("no finished turn {turn} to fork from")
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
    pub paid: bool,
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
    pub settings: Vec<Setting>,
    pub interrupts: Interrupts,
    pub wire: Option<Wire>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    Anthropic,
    Openai,
}

impl Wire {
    pub const SETTINGS: [&'static str; 4] = ["url", "headers", "head", "tail"];
    pub const PREAMBLE: &'static str = "preamble";

    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::Openai),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
        }
    }

    pub const fn answer(self) -> &'static str {
        match self {
            Self::Anthropic => "answer",
            Self::Openai => "message",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    pub name: String,
    pub ty: Type,
    pub default: Literal,
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
pub const ATTEMPT_INTERRUPTED: VariantId = 4;
pub const ATTEMPT_VARIANTS: [&str; 5] = ["ok", "throttled", "transient", "rejected", "interrupted"];
pub const ATTEMPT_REPLY: usize = 0;
pub const ATTEMPT_STOP: usize = 1;
pub const USAGE_FIELDS: [&str; 4] = ["input", "output", "cache_read", "cache_write"];
pub const ATTEMPT_USAGE: usize = 2;
pub const ATTEMPT_OK_FIELDS: usize = ATTEMPT_USAGE + USAGE_FIELDS.len();
pub const ATTEMPT_ERROR: usize = 0;
pub const ATTEMPT_RETRY_AFTER: usize = 1;

pub fn attempt_variants(stop: SumId, interruptible: bool) -> Vec<Variant> {
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
        Vec::new(),
    ];
    let count = if interruptible {
        ATTEMPT_VARIANTS.len()
    } else {
        ATTEMPT_INTERRUPTED
    };
    ATTEMPT_VARIANTS
        .iter()
        .zip(fields)
        .take(count)
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
    Hold(ConstId),
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
            Self::Hold(_) => "hold",
            Self::Suspend => "suspend",
            Self::Deny => "deny",
        }
    }

    pub const fn parks(self) -> bool {
        matches!(self, Self::Wait(_) | Self::Hold(_) | Self::Suspend)
    }

    pub const fn sleeps(self) -> Option<ConstId> {
        match self {
            Self::Wait(cap) | Self::Hold(cap) => Some(cap),
            _ => None,
        }
    }
}

pub const WAIT_TOOL: &str = "wait";
pub const HOLD_TOOL: &str = "hold";
pub const SUSPEND_TOOL: &str = "suspend";
pub const DENY_TOOL: &str = "deny";
pub const RESPOND_TOOL: &str = "respond";
pub const SESSION_STARTED_FIELD: &str = "_session_started";
pub const SESSION_ENDED_FIELD: &str = "_session_ended";
pub const HOOK_DECIDED_FIELD: &str = "_hook_decided";
pub const HOOK_DENIED_FIELD: &str = "_hook_denied";
pub const HOOK_RESPONDED_FIELD: &str = "_hook_responded";
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
    Rewrite {
        field: FieldId,
        arms: Vec<RewriteArm>,
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
        taken: Taken,
    },
    Finish {
        value: Expr,
    },
    Set {
        local: LocalId,
        value: Expr,
    },
    SetIndex {
        local: LocalId,
        index: Expr,
        value: Expr,
    },
    PushLocal {
        local: LocalId,
        value: Expr,
        append: bool,
    },
    While {
        index: LocalId,
        bound: Bound,
        condition: Expr,
        body: Vec<Stmt>,
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
    pub answer: Option<Answer>,
}

#[derive(Clone, Debug)]
pub struct Answer {
    pub prelude: Vec<Stmt>,
    pub decided: Expr,
    pub site: SiteId,
    pub args: Vec<Expr>,
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
        match &self.answer {
            None => body.push(self.dispatch()),
            Some(answer) => {
                body.extend(sequential_block(&answer.prelude));
                body.push(Stmt::If {
                    condition: answer.decided.clone(),
                    then: vec![Stmt::Effect {
                        site: answer.site,
                        args: answer.args.clone(),
                        result: self.result,
                    }],
                    otherwise: vec![self.dispatch()],
                });
            }
        }
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
        | Stmt::Rewrite { .. }
        | Stmt::Effect { .. }
        | Stmt::Take { .. }
        | Stmt::Finish { .. }
        | Stmt::Set { .. }
        | Stmt::SetIndex { .. }
        | Stmt::PushLocal { .. }
        | Stmt::While { .. } => statement.clone(),
    }
}

pub fn visit_exprs(statements: &[Stmt], visit: &mut dyn FnMut(&Expr)) {
    for statement in statements {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Push { value, .. }
            | Stmt::Set { value, .. }
            | Stmt::PushLocal { value, .. }
            | Stmt::Finish { value } => visit_expr(value, visit),
            Stmt::SetIndex { index, value, .. } => {
                visit_expr(index, visit);
                visit_expr(value, visit);
            }
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                visit_expr(condition, visit);
                visit_exprs(then, visit);
                visit_exprs(otherwise, visit);
            }
            Stmt::Match { subject, arms, .. } => {
                visit_expr(subject, visit);
                arms.iter().for_each(|arm| visit_exprs(&arm.body, visit));
            }
            Stmt::Loop { body, .. } => visit_exprs(body, visit),
            Stmt::While {
                condition, body, ..
            } => {
                visit_expr(condition, visit);
                visit_exprs(body, visit);
            }
            Stmt::For { list, body, .. } => {
                visit_expr(list, visit);
                visit_exprs(body, visit);
            }
            Stmt::Effect { args, .. } => args.iter().for_each(|arg| visit_expr(arg, visit)),
            Stmt::Shrink { .. }
            | Stmt::KeepBlobs { .. }
            | Stmt::Rewrite { .. }
            | Stmt::Parallel(_)
            | Stmt::Take { .. } => {}
        }
    }
}

pub fn visit_expr(value: &Expr, visit: &mut dyn FnMut(&Expr)) {
    visit(value);
    match value {
        Expr::Member { base: operand, .. }
        | Expr::Not(operand)
        | Expr::Len(operand)
        | Expr::TextLen(operand)
        | Expr::Truncate { text: operand, .. }
        | Expr::Copy { value: operand, .. } => visit_expr(operand, visit),
        Expr::Construct { fields: args, .. }
        | Expr::TextBuiltin { args, .. }
        | Expr::Call { args, .. }
        | Expr::Bytes { args, .. } => args.iter().for_each(|arg| visit_expr(arg, visit)),
        Expr::Binary { left, right, .. }
        | Expr::With {
            list: left,
            entry: right,
        }
        | Expr::Index {
            base: left,
            index: right,
        } => {
            visit_expr(left, visit);
            visit_expr(right, visit);
        }
        Expr::Outcome {
            tag,
            value,
            reason,
            original,
            ..
        } => [tag, value, reason, original]
            .into_iter()
            .for_each(|part| visit_expr(part, visit)),
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

pub fn runs_effects(statements: &[Stmt]) -> bool {
    statements.iter().any(|statement| match statement {
        Stmt::Effect { .. } | Stmt::Parallel(_) => true,
        Stmt::If {
            then, otherwise, ..
        } => runs_effects(then) || runs_effects(otherwise),
        Stmt::Match { arms, .. } => arms.iter().any(|arm| runs_effects(&arm.body)),
        Stmt::Loop { body, .. } | Stmt::For { body, .. } => runs_effects(body),
        _ => false,
    })
}

fn lanes_run_effects(statements: &[Stmt]) -> bool {
    statements.iter().any(|statement| match statement {
        Stmt::Parallel(parallel) => runs_effects(&parallel.body),
        Stmt::If {
            then, otherwise, ..
        } => lanes_run_effects(then) || lanes_run_effects(otherwise),
        Stmt::Match { arms, .. } => arms.iter().any(|arm| lanes_run_effects(&arm.body)),
        Stmt::Loop { body, .. } | Stmt::For { body, .. } => lanes_run_effects(body),
        _ => false,
    })
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
        | Stmt::Rewrite { .. }
        | Stmt::Effect { .. }
        | Stmt::Take { .. }
        | Stmt::Finish { .. }
        | Stmt::Set { .. }
        | Stmt::SetIndex { .. }
        | Stmt::PushLocal { .. }
        | Stmt::While { .. } => false,
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
        Expr::Construct { fields, .. }
        | Expr::TextBuiltin { args: fields, .. }
        | Expr::Call { args: fields, .. }
        | Expr::Bytes { args: fields, .. } => fields
            .iter()
            .for_each(|field| expr_cuts(field, constants, out)),
        Expr::Copy { value, .. } => expr_cuts(value, constants, out),
        Expr::Outcome {
            tag,
            value,
            reason,
            original,
            ..
        } => [tag, value, reason, original]
            .into_iter()
            .for_each(|part| expr_cuts(part, constants, out)),
        Expr::Binary { left, right, .. }
        | Expr::With {
            list: left,
            entry: right,
        }
        | Expr::Index {
            base: left,
            index: right,
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
            | Stmt::Set { value, .. }
            | Stmt::PushLocal { value, .. }
            | Stmt::Finish { value } => expr_cuts(value, constants, out),
            Stmt::SetIndex { index, value, .. } => {
                expr_cuts(index, constants, out);
                expr_cuts(value, constants, out);
            }
            Stmt::While {
                condition, body, ..
            } => {
                expr_cuts(condition, constants, out);
                block_cuts(body, constants, out);
            }
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
                if let Some(answer) = &parallel.answer {
                    block_cuts(&answer.prelude, constants, out);
                    expr_cuts(&answer.decided, constants, out);
                    answer
                        .args
                        .iter()
                        .for_each(|arg| expr_cuts(arg, constants, out));
                }
            }
            Stmt::Effect { args, .. } => args.iter().for_each(|arg| expr_cuts(arg, constants, out)),
            Stmt::Rewrite { arms, .. } => {
                for arm in arms {
                    if let Rewritten::Into(entry) = &arm.entry {
                        expr_cuts(entry, constants, out);
                    }
                }
            }
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
        for handler in self
            .handlers
            .iter()
            .chain(&self.sides)
            .chain(&self.forked)
            .chain(&self.functions)
        {
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

    pub fn lanes_run_effects(&self) -> bool {
        self.handlers
            .iter()
            .any(|handler| lanes_run_effects(&handler.body))
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
pub struct RewriteArm {
    pub variant: VariantId,
    pub bindings: Vec<Option<LocalId>>,
    pub entry: Rewritten,
}

#[derive(Clone, Debug)]
pub enum Rewritten {
    Kept,
    Into(Expr),
    Dropped,
}

pub fn rewrite_drops(arms: &[RewriteArm]) -> bool {
    arms.iter()
        .any(|arm| matches!(arm.entry, Rewritten::Dropped))
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
    Outcome {
        ty: Type,
        tag: Box<Expr>,
        value: Box<Expr>,
        reason: Box<Expr>,
        original: Box<Expr>,
    },
    Call {
        function: FunctionId,
        args: Vec<Expr>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Bytes {
        op: BytesOp,
        args: Vec<Expr>,
    },
    Copy {
        value: Box<Expr>,
        bytes: bool,
    },
}

pub type FunctionId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BytesOp {
    Get,
    Slice,
    OfText,
    ToText,
    OfHex,
    ToHex,
    Empty,
    EmptyList,
}

impl BytesOp {
    pub const NAMED: [Self; 5] = [
        Self::OfText,
        Self::ToText,
        Self::Slice,
        Self::OfHex,
        Self::ToHex,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Slice => "slice",
            Self::OfText => "bytes_of",
            Self::ToText => "text_of",
            Self::OfHex => "bytes_of_hex",
            Self::ToHex => "hex_of",
            Self::Empty | Self::EmptyList => "[]",
        }
    }

    pub const fn arity(self) -> usize {
        match self {
            Self::Empty | Self::EmptyList => 0,
            Self::ToText | Self::ToHex => 1,
            Self::Get | Self::OfText | Self::OfHex => 2,
            Self::Slice => 3,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::NAMED.into_iter().find(|op| op.name() == name)
    }
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
    WithPayment,
    JsonEscape,
}

pub const TEXT_WHITESPACE: [char; 5] = [' ', '\t', '\n', '\x0c', '\r'];

impl TextOp {
    pub const ALL: [Self; 5] = [
        Self::Contains,
        Self::Before,
        Self::After,
        Self::Trim,
        Self::JsonEscape,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Before => "before",
            Self::After => "after",
            Self::Trim => "trim",
            Self::WithPayment => "_with_payment",
            Self::JsonEscape => "json_escape",
        }
    }

    pub const fn arity(self) -> usize {
        match self {
            Self::Contains | Self::Before | Self::After | Self::WithPayment => 2,
            Self::Trim | Self::JsonEscape => 1,
        }
    }

    pub fn result(self) -> Type {
        match self {
            Self::Contains => Type::Bool,
            Self::Before | Self::After | Self::Trim | Self::WithPayment | Self::JsonEscape => {
                Type::Text
            }
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.name() == name)
    }
}

pub const JSON_CONTROL_END: u32 = 0x20;

pub fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if u32::from(control) < JSON_CONTROL_END => {
                out.push_str(&format!("\\u{:04x}", u32::from(control)));
            }
            other => out.push(other),
        }
    }
    out
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
        (TextOp::WithPayment, Some(payment)) => TextValue::Text(with_payment(subject, payment)),
        (TextOp::Trim, None) => {
            TextValue::Text(subject.trim_matches(TEXT_WHITESPACE.as_slice()).to_owned())
        }
        (TextOp::JsonEscape, None) => TextValue::Text(json_escape(subject)),
        (op, separator) => panic!(
            "text builtin {} takes {} argument(s), given a separator: {}",
            op.name(),
            op.arity(),
            separator.is_some()
        ),
    }
}
