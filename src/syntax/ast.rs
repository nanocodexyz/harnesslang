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
    Model(Model),
    State(State),
    Invariant(Invariant),
    Handler(Handler),
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
    pub returns: TypeExpr,
    pub effect: EffectClass,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub name: Name,
    pub calls: Name,
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
pub struct Handler {
    pub name: Name,
    pub params: Vec<Param>,
    pub returns: TypeExpr,
    pub requires: Option<Expr>,
    pub body: Block,
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
}

impl TypeExpr {
    pub fn at(&self) -> Position {
        match self {
            Self::Named(name) => name.at,
            Self::List { at, .. } | Self::Known { at, .. } | Self::Maybe { at, .. } => *at,
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
        body: Block,
    },
    Return {
        at: Position,
        value: Expr,
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
}

impl Expr {
    pub fn at(&self) -> Position {
        match self {
            Self::Number(_, at) | Self::Text(_, at) | Self::Bool(_, at) => *at,
            Self::Name(name) => name.at,
            Self::Member { base, .. } => base.at(),
            Self::Call { callee, .. } => callee.at,
            Self::Binary { at, .. } | Self::Not { at, .. } => *at,
        }
    }
}
