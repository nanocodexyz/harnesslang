use crate::effect::EffectClass;
use crate::syntax::ast::{
    Arm, BinaryOp, Block, Const, Expr, Handler, Invariant, Item, Model, Name, Param, Program,
    State, Stmt, Tool, TypeDef, TypeExpr, VariantDef,
};
use crate::syntax::lexer::{Lexeme, Symbol, Token};
use crate::syntax::{Diagnostic, Position};

pub const NESTING_DEPTH_MAX: u32 = 64;

pub const KEYWORDS: [&str; 24] = [
    "harness",
    "const",
    "type",
    "tool",
    "model",
    "state",
    "on",
    "let",
    "push",
    "if",
    "else",
    "match",
    "loop",
    "for",
    "return",
    "true",
    "false",
    "and",
    "or",
    "not",
    "list",
    "effect",
    "invariant",
    "requires",
];

const ITEMS: &str = "'const', 'type', 'tool', 'model', 'state', 'invariant' or 'on'";

pub struct Parser<'a> {
    lexemes: &'a [Lexeme],
    position: usize,
    depth: u32,
}

type Parsed<T> = Result<T, Diagnostic>;

impl<'a> Parser<'a> {
    pub fn new(lexemes: &'a [Lexeme]) -> Self {
        assert!(
            matches!(lexemes.last().map(|lexeme| &lexeme.token), Some(Token::End)),
            "the lexer ends every token stream with End"
        );
        Self {
            lexemes,
            position: 0,
            depth: 0,
        }
    }

    fn peek(&self) -> &Lexeme {
        &self.lexemes[self.position]
    }

    fn peek_second(&self) -> &Token {
        let index = (self.position + 1).min(self.lexemes.len() - 1);
        &self.lexemes[index].token
    }

    fn advance(&mut self) -> Lexeme {
        let lexeme = self.lexemes[self.position].clone();
        if self.position + 1 < self.lexemes.len() {
            self.position += 1;
        }
        lexeme
    }

    fn at(&self) -> Position {
        self.peek().at
    }

    fn unexpected(&self, wanted: &str) -> Diagnostic {
        let found = self.peek().token.describe();
        Diagnostic::new(self.at(), format!("expected {wanted}, found {found}"))
    }

    fn is_symbol(&self, symbol: Symbol) -> bool {
        self.peek().token == Token::Symbol(symbol)
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(&self.peek().token, Token::Name(name) if name == word)
    }

    fn symbol(&mut self, symbol: Symbol) -> Parsed<Position> {
        if !self.is_symbol(symbol) {
            return Err(self.unexpected(&format!("'{}'", symbol.text())));
        }
        Ok(self.advance().at)
    }

    fn word(&mut self, word: &str) -> Parsed<Position> {
        if !self.is_word(word) {
            return Err(self.unexpected(&format!("'{word}'")));
        }
        Ok(self.advance().at)
    }

    fn name(&mut self, what: &str) -> Parsed<Name> {
        let at = self.at();
        match &self.peek().token {
            Token::Name(text) if KEYWORDS.contains(&text.as_str()) => Err(Diagnostic::new(
                at,
                format!("expected {what}, found keyword '{text}'"),
            )),
            Token::Name(text) => {
                let text = text.clone();
                self.advance();
                Ok(Name { text, at })
            }
            _ => Err(self.unexpected(what)),
        }
    }

    fn bound(&mut self, what: &str) -> Parsed<Name> {
        if let Token::Number(value) = self.peek().token {
            return Err(Diagnostic::new(
                self.at(),
                format!("{what} must be a named constant, found number {value}"),
            ));
        }
        self.name(&format!("a named constant for {what}"))
    }

    fn enter(&mut self) -> Parsed<()> {
        if self.depth >= NESTING_DEPTH_MAX {
            return Err(Diagnostic::new(
                self.at(),
                format!("nesting is deeper than nesting_depth_max ({NESTING_DEPTH_MAX})"),
            ));
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        assert!(self.depth > 0, "every leave follows an enter");
        self.depth -= 1;
    }

    pub fn program(mut self) -> Parsed<Program> {
        self.word("harness")?;
        let name = self.name("the harness name")?;
        let mut items = Vec::new();
        while self.peek().token != Token::End {
            items.push(self.item()?);
        }
        assert_eq!(
            self.depth, 0,
            "a whole program leaves every nesting it enters"
        );
        Ok(Program { name, items })
    }

    fn item(&mut self) -> Parsed<Item> {
        let Token::Name(word) = &self.peek().token else {
            return Err(self.unexpected(ITEMS));
        };
        match word.as_str() {
            "const" => self.constant().map(Item::Const),
            "type" => self.type_def().map(Item::Type),
            "tool" => self.tool().map(Item::Tool),
            "model" => self.model().map(Item::Model),
            "state" => self.state().map(Item::State),
            "invariant" => self.invariant().map(Item::Invariant),
            "on" => self.handler().map(Item::Handler),
            _ => Err(self.unexpected(ITEMS)),
        }
    }

    fn invariant(&mut self) -> Parsed<Invariant> {
        self.word("invariant")?;
        let name = self.name("an invariant name")?;
        self.symbol(Symbol::Colon)?;
        let condition = self.expr()?;
        Ok(Invariant { name, condition })
    }

    fn constant(&mut self) -> Parsed<Const> {
        self.word("const")?;
        let name = self.name("a constant name")?;
        self.symbol(Symbol::Colon)?;
        let ty = self.type_expr()?;
        self.symbol(Symbol::Assign)?;
        let Token::Number(value) = self.peek().token else {
            return Err(self.unexpected("a number"));
        };
        self.advance();
        Ok(Const { name, ty, value })
    }

    fn type_def(&mut self) -> Parsed<TypeDef> {
        self.word("type")?;
        let name = self.name("a type name")?;
        self.symbol(Symbol::Assign)?;
        let mut variants = vec![self.variant()?];
        while self.is_symbol(Symbol::Bar) {
            self.advance();
            variants.push(self.variant()?);
        }
        Ok(TypeDef { name, variants })
    }

    fn variant(&mut self) -> Parsed<VariantDef> {
        let name = self.name("a variant name")?;
        let mut fields = Vec::new();
        if self.is_symbol(Symbol::LeftParen) {
            self.advance();
            loop {
                fields.push(self.type_expr()?);
                if self.is_symbol(Symbol::Comma) {
                    self.advance();
                    continue;
                }
                self.symbol(Symbol::RightParen)?;
                break;
            }
        }
        Ok(VariantDef { name, fields })
    }

    fn tool(&mut self) -> Parsed<Tool> {
        self.word("tool")?;
        let name = self.name("a tool name")?;
        let params = self.params()?;
        self.symbol(Symbol::Arrow)?;
        let returns = self.type_expr()?;
        self.word("effect")?;
        let at = self.at();
        let effect = match &self.peek().token {
            Token::Name(word) => EffectClass::parse(word),
            _ => None,
        }
        .ok_or_else(|| {
            Diagnostic::new(
                at,
                format!(
                    "expected 'none', 'idempotent' or 'external', found {}",
                    self.peek().token.describe()
                ),
            )
        })?;
        self.advance();
        Ok(Tool {
            name,
            params,
            returns,
            effect,
        })
    }

    fn model(&mut self) -> Parsed<Model> {
        self.word("model")?;
        let name = self.name("a model name")?;
        self.word("calls")?;
        let calls = self.bound("the calls in one model reply")?;
        Ok(Model { name, calls })
    }

    fn state(&mut self) -> Parsed<State> {
        let at = self.word("state")?;
        self.symbol(Symbol::LeftBrace)?;
        let mut fields = Vec::new();
        while !self.is_symbol(Symbol::RightBrace) {
            let name = self.name("a state field name")?;
            self.symbol(Symbol::Colon)?;
            let ty = self.type_expr()?;
            fields.push(Param { name, ty });
        }
        self.advance();
        Ok(State { at, fields })
    }

    fn handler(&mut self) -> Parsed<Handler> {
        self.word("on")?;
        let name = self.name("a handler name")?;
        let params = self.params()?;
        self.symbol(Symbol::Arrow)?;
        let returns = self.type_expr()?;
        let requires = if self.is_word("requires") {
            self.advance();
            Some(self.expr()?)
        } else {
            None
        };
        let body = self.block()?;
        Ok(Handler {
            name,
            params,
            returns,
            requires,
            body,
        })
    }

    fn params(&mut self) -> Parsed<Vec<Param>> {
        self.symbol(Symbol::LeftParen)?;
        let mut params = Vec::new();
        if self.is_symbol(Symbol::RightParen) {
            self.advance();
            return Ok(params);
        }
        loop {
            let name = self.name("a parameter name")?;
            self.symbol(Symbol::Colon)?;
            let ty = self.type_expr()?;
            params.push(Param { name, ty });
            if self.is_symbol(Symbol::Comma) {
                self.advance();
                continue;
            }
            self.symbol(Symbol::RightParen)?;
            return Ok(params);
        }
    }

    fn type_expr(&mut self) -> Parsed<TypeExpr> {
        self.enter()?;
        let at = self.at();
        let parsed = if self.is_word("list") {
            self.advance();
            self.symbol(Symbol::Less)?;
            let element = Box::new(self.type_expr()?);
            if self.is_symbol(Symbol::Greater) {
                return Err(Diagnostic::new(
                    self.at(),
                    "a list needs a capacity: write list<T, constant>",
                ));
            }
            self.symbol(Symbol::Comma)?;
            let capacity = self.bound("a list's capacity")?;
            self.symbol(Symbol::Greater)?;
            TypeExpr::List {
                at,
                element,
                capacity,
            }
        } else if (self.is_word("known") || self.is_word("maybe"))
            && self.peek_second() == &Token::Symbol(Symbol::Less)
        {
            let known = self.is_word("known");
            self.advance();
            self.symbol(Symbol::Less)?;
            let inner = Box::new(self.type_expr()?);
            self.symbol(Symbol::Greater)?;
            if known {
                TypeExpr::Known { at, inner }
            } else {
                TypeExpr::Maybe { at, inner }
            }
        } else {
            TypeExpr::Named(self.name("a type")?)
        };
        self.leave();
        Ok(parsed)
    }

    fn block(&mut self) -> Parsed<Block> {
        self.enter()?;
        let at = self.symbol(Symbol::LeftBrace)?;
        let mut statements = Vec::new();
        while !self.is_symbol(Symbol::RightBrace) {
            if self.peek().token == Token::End {
                return Err(Diagnostic::new(
                    at,
                    "block is not closed before the end of the file",
                ));
            }
            statements.push(self.statement()?);
        }
        self.advance();
        self.leave();
        Ok(Block { at, statements })
    }

    fn statement(&mut self) -> Parsed<Stmt> {
        let at = self.at();
        let Token::Name(word) = &self.peek().token else {
            return Err(self.unexpected("a statement"));
        };
        match word.as_str() {
            "let" => {
                self.advance();
                let name = self.name("a binding name")?;
                self.symbol(Symbol::Assign)?;
                let value = self.expr()?;
                Ok(Stmt::Let { name, value })
            }
            "push" => {
                self.advance();
                let list = self.name("a list to push onto")?;
                let value = self.expr()?;
                Ok(Stmt::Push { at, list, value })
            }
            "if" => self.if_statement(),
            "match" => self.match_statement(),
            "loop" => {
                self.advance();
                let name = self.name("a loop name")?;
                self.word("below")?;
                let bound = self.bound(&format!("the bound of loop '{}'", name.text))?;
                let body = self.block()?;
                Ok(Stmt::Loop {
                    at,
                    name,
                    bound,
                    body,
                })
            }
            "for" => {
                self.advance();
                let name = self.name("a loop variable name")?;
                self.word("in")?;
                let list = self.expr()?;
                let body = self.block()?;
                Ok(Stmt::For {
                    at,
                    name,
                    list,
                    body,
                })
            }
            "return" => {
                self.advance();
                let value = self.expr()?;
                Ok(Stmt::Return { at, value })
            }
            _ => {
                let name = self.name("a statement")?;
                if !self.is_symbol(Symbol::Assign) {
                    return Err(self.unexpected("'=' after a name that starts a statement"));
                }
                self.advance();
                let value = self.expr()?;
                Ok(Stmt::Assign { name, value })
            }
        }
    }

    fn if_statement(&mut self) -> Parsed<Stmt> {
        self.enter()?;
        let at = self.word("if")?;
        let condition = self.expr()?;
        let then = self.block()?;
        let otherwise = if self.is_word("else") {
            let else_at = self.advance().at;
            if self.is_word("if") {
                let nested = self.if_statement()?;
                Some(Block {
                    at: else_at,
                    statements: vec![nested],
                })
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        self.leave();
        Ok(Stmt::If {
            at,
            condition,
            then,
            otherwise,
        })
    }

    fn match_statement(&mut self) -> Parsed<Stmt> {
        let at = self.word("match")?;
        let subject = self.expr()?;
        self.symbol(Symbol::LeftBrace)?;
        let mut arms = Vec::new();
        while !self.is_symbol(Symbol::RightBrace) {
            let variant = self.name("a variant pattern")?;
            let mut bindings = Vec::new();
            if self.is_symbol(Symbol::LeftParen) {
                self.advance();
                loop {
                    bindings.push(self.name("a binding name")?);
                    if self.is_symbol(Symbol::Comma) {
                        self.advance();
                        continue;
                    }
                    self.symbol(Symbol::RightParen)?;
                    break;
                }
            }
            self.symbol(Symbol::FatArrow)?;
            let body = self.block()?;
            arms.push(Arm {
                variant,
                bindings,
                body,
            });
            if self.is_symbol(Symbol::Comma) {
                self.advance();
            }
        }
        self.advance();
        Ok(Stmt::Match { at, subject, arms })
    }

    pub fn expr(&mut self) -> Parsed<Expr> {
        self.enter()?;
        let parsed = self.or();
        self.leave();
        parsed
    }

    fn binary_chain(
        &mut self,
        next: fn(&mut Self) -> Parsed<Expr>,
        operator: fn(&Self) -> Option<BinaryOp>,
    ) -> Parsed<Expr> {
        let mut left = next(self)?;
        let mut entered = 0;
        while let Some(op) = operator(self) {
            self.enter()?;
            entered += 1;
            let at = self.advance().at;
            let right = next(self)?;
            left = Expr::Binary {
                at,
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        for _ in 0..entered {
            self.leave();
        }
        Ok(left)
    }

    fn or(&mut self) -> Parsed<Expr> {
        self.binary_chain(Self::and, |parser| {
            parser.is_word("or").then_some(BinaryOp::Or)
        })
    }

    fn and(&mut self) -> Parsed<Expr> {
        self.binary_chain(Self::not, |parser| {
            parser.is_word("and").then_some(BinaryOp::And)
        })
    }

    fn not(&mut self) -> Parsed<Expr> {
        if self.is_word("not") {
            self.enter()?;
            let at = self.advance().at;
            let operand = Box::new(self.not()?);
            self.leave();
            return Ok(Expr::Not { at, operand });
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Parsed<Expr> {
        let left = self.sum()?;
        let op = match self.peek().token {
            Token::Symbol(Symbol::Equal) => BinaryOp::Equal,
            Token::Symbol(Symbol::NotEqual) => BinaryOp::NotEqual,
            Token::Symbol(Symbol::Less) => BinaryOp::Less,
            Token::Symbol(Symbol::LessEqual) => BinaryOp::LessEqual,
            Token::Symbol(Symbol::Greater) => BinaryOp::Greater,
            Token::Symbol(Symbol::GreaterEqual) => BinaryOp::GreaterEqual,
            _ => return Ok(left),
        };
        let at = self.advance().at;
        let right = self.sum()?;
        Ok(Expr::Binary {
            at,
            op,
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    fn sum(&mut self) -> Parsed<Expr> {
        self.binary_chain(Self::product, |parser| match parser.peek().token {
            Token::Symbol(Symbol::Plus) => Some(BinaryOp::Add),
            Token::Symbol(Symbol::Minus) => Some(BinaryOp::Subtract),
            Token::Symbol(Symbol::PlusPlus) => Some(BinaryOp::Concat),
            _ => None,
        })
    }

    fn product(&mut self) -> Parsed<Expr> {
        self.binary_chain(Self::postfix, |parser| {
            (parser.peek().token == Token::Symbol(Symbol::Star)).then_some(BinaryOp::Multiply)
        })
    }

    fn postfix(&mut self) -> Parsed<Expr> {
        let mut expr = self.primary()?;
        let mut entered = 0;
        while self.is_symbol(Symbol::Dot) {
            self.enter()?;
            entered += 1;
            self.advance();
            let at = self.at();
            let Token::Name(text) = self.peek().token.clone() else {
                return Err(self.unexpected("a field name"));
            };
            self.advance();
            let field = Name { text, at };
            expr = Expr::Member {
                base: Box::new(expr),
                field,
            };
        }
        for _ in 0..entered {
            self.leave();
        }
        Ok(expr)
    }

    fn primary(&mut self) -> Parsed<Expr> {
        let at = self.at();
        match self.peek().token.clone() {
            Token::Number(value) => {
                self.advance();
                Ok(Expr::Number(value, at))
            }
            Token::Text(text) => {
                self.advance();
                Ok(Expr::Text(text, at))
            }
            Token::Name(word) if word == "true" || word == "false" => {
                self.advance();
                Ok(Expr::Bool(word == "true", at))
            }
            Token::Symbol(Symbol::LeftParen) => {
                self.advance();
                let inner = self.expr()?;
                self.symbol(Symbol::RightParen)?;
                Ok(inner)
            }
            Token::Name(_) => {
                let callee = self.name("an expression")?;
                if !self.is_symbol(Symbol::LeftParen) {
                    return Ok(Expr::Name(callee));
                }
                self.advance();
                let mut args = Vec::new();
                if self.is_symbol(Symbol::RightParen) {
                    self.advance();
                    return Ok(Expr::Call { callee, args });
                }
                loop {
                    args.push(self.expr()?);
                    if self.is_symbol(Symbol::Comma) {
                        self.advance();
                        continue;
                    }
                    self.symbol(Symbol::RightParen)?;
                    return Ok(Expr::Call { callee, args });
                }
            }
            _ => Err(self.unexpected("an expression")),
        }
    }
}
