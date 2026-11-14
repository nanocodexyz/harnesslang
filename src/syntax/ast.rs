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
    Listing(Listing),
    Model(Model),
    State(State),
    Invariant(Invariant),
    Function(Function),
    Handler(Handler),
    Event(Event),
    Hook(Hook),
    Spawn(Spawn),
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
}

impl HookKind {
    pub const ALL: [Self; 9] = [
        Self::BeforeTool,
        Self::AfterTool,
        Self::SessionStart,
        Self::PromptSubmit,
        Self::PreCompact,
        Self::PostCompact,
        Self::Stop,
        Self::SubagentStart,
        Self::SubagentStop,
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
    pub finds: Option<Name>,
}

#[derive(Clone, Debug)]
pub struct Code {
    pub name: Name,
    pub tools: Vec<Name>,
    pub fields: Vec<CodeField>,
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
}

#[derive(Clone, Debug)]
pub struct Model {
    pub name: Name,
    pub calls: Name,
    pub reply: Option<Name>,
    pub modes: Vec<Name>,
    pub window: Option<Name>,
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
    },
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pattern: Pattern,
    pub body: Block,
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
