use crate::effect::EffectClass;
use crate::syntax::Position;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    pub at: Position,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub name: Name,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub enum Item {
    Const(Const),
    Type(TypeDef),
    Tool(Tool),
    Code(Code),
    Wallet(Wallet),
    Listing(Listing),
    Model(Model),
    State(State),
    Invariant(Invariant),
    Function(Function),
    Handler(Handler),
    Event(Event),
    Hook(Hook),
    Spawn(Spawn),
    Fork(Fork),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HookKind {
    BeforeTool,
    AfterTool,
    SessionStart,
    PromptSubmit,
    PreCompact,
    PostCompact,
    Stop,
    SubagentStart,
    SubagentStop,
    Forked,
}

impl HookKind {
    pub const ALL: [Self; 10] = [
        Self::BeforeTool,
        Self::AfterTool,
        Self::SessionStart,
        Self::PromptSubmit,
        Self::PreCompact,
        Self::PostCompact,
        Self::Stop,
        Self::SubagentStart,
        Self::SubagentStop,
        Self::Forked,
    ];

    pub const fn words(self) -> (&'static str, &'static str) {
        match self {
            Self::BeforeTool => ("before", "tool"),
            Self::AfterTool => ("after", "tool"),
            Self::SessionStart => ("session", "start"),
            Self::PromptSubmit => ("prompt", "submit"),
            Self::PreCompact => ("pre", "compact"),
            Self::PostCompact => ("post", "compact"),
            Self::Stop => ("stop", ""),
            Self::SubagentStart => ("subagent", "start"),
            Self::SubagentStop => ("subagent", "stop"),
            Self::Forked => ("forked", ""),
        }
    }

    pub fn describe(self) -> String {
        match self.words() {
            (first, "") => format!("on {first}"),
            (first, second) if matches!(self, Self::BeforeTool | Self::AfterTool) => {
                format!("{first} {second}")
            }
            (first, second) => format!("on {first} {second}"),
        }
    }

    pub const fn denies(self) -> bool {
        matches!(self, Self::BeforeTool | Self::AfterTool)
    }
}

#[derive(Clone, Debug)]
pub struct Hook {
    pub kind: HookKind,
    pub at: Position,
    pub params: Vec<Param>,
    pub body: Block,
}

#[derive(Clone, Debug)]
pub struct Spawn {
    pub name: Name,
    pub handler: Name,
    pub file: Name,
    pub sessions: Option<Name>,
}

#[derive(Clone, Debug)]
pub struct Fork {
    pub name: Name,
    pub at: Name,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub name: Name,
    pub params: Vec<Param>,
    pub bound: Option<Name>,
}

#[derive(Clone, Debug)]
pub struct Invariant {
    pub name: Name,
    pub condition: Expr,
}

#[derive(Clone, Debug)]
pub struct Const {
    pub name: Name,
    pub ty: TypeExpr,
    pub value: u64,
}

#[derive(Clone, Debug)]
pub struct TypeDef {
    pub name: Name,
    pub body: TypeBody,
}

#[derive(Clone, Debug)]
pub enum TypeBody {
    Variants(Vec<VariantDef>),
    Values(Vec<Name>),
}

#[derive(Clone, Debug)]
pub struct VariantDef {
    pub name: Name,
    pub fields: Vec<TypeExpr>,
}

#[derive(Clone, Debug)]
pub struct Tool {
    pub name: Name,
    pub params: Vec<Param>,
    pub defaults: Vec<Option<Expr>>,
    pub returns: TypeExpr,
    pub attaches: Option<TypeExpr>,
    pub effect: EffectClass,
    pub recoverable: Option<Position>,
    pub interrupts: Vec<Name>,
    pub jobs: Option<Name>,
    pub finds: Option<Name>,
    pub paid: Option<Name>,
}

#[derive(Clone, Debug)]
pub struct Wallet {
    pub name: Name,
    pub spend: Name,
    pub charge: Name,
    pub payments: Name,
}

#[derive(Clone, Debug)]
pub struct Code {
    pub name: Name,
    pub tools: Vec<Name>,
    pub fields: Vec<CodeField>,
    pub interrupts: Vec<Name>,
}

#[derive(Clone, Debug)]
pub struct CodeField {
    pub key: Name,
    pub value: Name,
}

#[derive(Clone, Debug)]
pub struct Listing {
    pub name: Name,
    pub bound: Name,
    pub params: Vec<Param>,
    pub defaults: Vec<Option<Expr>>,
    pub effect: EffectClass,
    pub paid: Option<Name>,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub name: Name,
    pub calls: Name,
    pub reply: Option<Name>,
    pub modes: Vec<Name>,
    pub settings: Vec<Param>,
    pub setting_defaults: Vec<Option<Expr>>,
    pub window: Option<Name>,
    pub interrupts: Vec<Name>,
    pub wire: Option<Name>,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub at: Position,
    pub fields: Vec<(Name, Expr)>,
}

#[derive(Clone, Debug)]
pub struct State {
    pub at: Position,
    pub fields: Vec<Param>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Name,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: Name,
    pub params: Vec<Param>,
    pub body: Block,
}

#[derive(Clone, Debug)]
pub struct Handler {
    pub name: Name,
    pub params: Vec<Param>,
    pub returns: TypeExpr,
    pub requires: Option<Expr>,
    pub body: Block,
    pub ends_session: bool,
    pub side: bool,
    pub idle: Option<Idle>,
}

#[derive(Clone, Debug)]
pub struct Idle {
    pub due: Name,
    pub bound: Name,
}

#[derive(Clone, Debug)]
pub enum TypeExpr {
    Named(Name),
    List {
        at: Position,
        element: Box<TypeExpr>,
        capacity: Name,
    },
    Known {
        at: Position,
        inner: Box<TypeExpr>,
    },
    Maybe {
        at: Position,
        inner: Box<TypeExpr>,
    },
    Blob {
        at: Position,
        bytes: Name,
    },
}

impl TypeExpr {
    pub fn at(&self) -> Position {
        match self {
            Self::Named(name) => name.at,
            Self::List { at, .. }
            | Self::Known { at, .. }
            | Self::Maybe { at, .. }
            | Self::Blob { at, .. } => *at,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub at: Position,
    pub statements: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let {
        name: Name,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    Assign {
        name: Name,
        value: Expr,
    },
    Push {
        at: Position,
        list: Name,
        value: Expr,
    },
    Clear {
        at: Position,
        list: Name,
    },
    Keep {
        at: Position,
        list: Name,
        count: Expr,
        first: Option<Expr>,
    },
    Rewrite {
        at: Position,
        list: Name,
        arms: Vec<RewriteArm>,
    },
    KeepBlobs {
        at: Position,
        list: Name,
        count: Name,
    },
    If {
        at: Position,
        condition: Expr,
        then: Block,
        otherwise: Option<Block>,
    },
    Match {
        at: Position,
        subject: Expr,
        arms: Vec<Arm>,
    },
    Loop {
        at: Position,
        name: Name,
        bound: Name,
        body: Block,
    },
    For {
        at: Position,
        name: Name,
        list: Expr,
        parallel: bool,
        body: Block,
    },
    Return {
        at: Position,
        value: Expr,
    },
    Call {
        callee: Name,
        args: Vec<Expr>,
    },
    Inlined {
        at: Position,
        function: Name,
        arguments: Vec<(Param, Expr)>,
        body: Block,
        sees_caller: bool,
    },
    Climb(Box<Climb>),
    Yield {
        at: Position,
        kind: Yield,
    },
}

#[derive(Clone, Debug)]
pub struct Climb {
    pub at: Position,
    pub name: Name,
    pub over: Name,
    pub from: Option<Expr>,
    pub ledger: Name,
    pub end: Option<Name>,
    pub cycles: Name,
    pub candidates: Option<Name>,
    pub trials: Name,
    pub cost: Option<ClimbCost>,
    pub wall: Option<Name>,
    pub failures: Option<Name>,
    pub metrics: Vec<Metric>,
    pub gates: Vec<Expr>,
    pub rule: Rule,
    pub propose: ClimbBlock,
    pub measure: ClimbBlock,
}

#[derive(Clone, Debug)]
pub struct ClimbCost {
    pub budget: Name,
    pub propose: Name,
    pub trial: Name,
}

#[derive(Clone, Debug)]
pub struct Metric {
    pub name: Name,
    pub higher: bool,
    pub max: Name,
    pub within: Option<Name>,
}

#[derive(Clone, Debug)]
pub enum Rule {
    Mean(Name),
    Sign(Name),
    Best(Name),
    When { names: Vec<Name>, condition: Expr },
}

#[derive(Clone, Debug)]
pub struct ClimbBlock {
    pub param: Name,
    pub body: Block,
}

#[derive(Clone, Debug)]
pub enum Yield {
    Candidate { value: Expr, hypothesis: Expr },
    Score(Vec<Expr>),
    Cost(Expr),
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pattern: Pattern,
    pub body: Block,
}

#[derive(Clone, Debug)]
pub struct RewriteArm {
    pub pattern: Pattern,
    pub entry: Option<Expr>,
}

#[derive(Clone, Debug)]
pub enum Pattern {
    Variant { name: Name, bindings: Vec<Name> },
    Value(Name),
}

impl Pattern {
    pub const fn name(&self) -> &Name {
        match self {
            Self::Variant { name, .. } | Self::Value(name) => name,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Concat,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
}

impl BinaryOp {
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
            Self::Concat => "++",
            Self::Equal => "==",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
            Self::And => "and",
            Self::Or => "or",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Expr {
    Number(u64, Position),
    Text(String, Position),
    Bool(bool, Position),
    Name(Name),
    Member {
        base: Box<Expr>,
        field: Name,
    },
    Call {
        callee: Name,
        args: Vec<Expr>,
        mode: Option<Name>,
        settings: Option<Settings>,
    },
    Binary {
        at: Position,
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Not {
        at: Position,
        operand: Box<Expr>,
    },
    Take {
        at: Position,
        events: Name,
    },
    With {
        at: Position,
        list: Box<Expr>,
        entry: Box<Expr>,
    },
}

impl Expr {
    pub fn at(&self) -> Position {
        match self {
            Self::Number(_, at) | Self::Text(_, at) | Self::Bool(_, at) => *at,
            Self::Name(name) => name.at,
            Self::Member { base, .. } => base.at(),
            Self::Call { callee, .. } => callee.at,
            Self::Binary { at, .. }
            | Self::Not { at, .. }
            | Self::Take { at, .. }
            | Self::With { at, .. } => *at,
        }
    }
}
