use crate::effect::EffectClass;
use crate::syntax::ast::{
    Arm, BinaryOp, Block, Code, CodeField, Const, Event, Expr, Function, Handler, Hook, HookKind,
    Invariant, Item, Listing, Model, Name, Param, Pattern, Program, Spawn, State, Stmt, Tool,
    TypeBody, TypeDef, TypeExpr, VariantDef,
};
use crate::syntax::lexer::{Lexeme, Symbol, Token};
use crate::syntax::{Diagnostic, Position};

pub const NESTING_DEPTH_MAX: u32 = 64;

pub const KEYWORDS: [&str; 27] = [
    "harness",
    "code",
    "const",
    "type",
    "tool",
    "model",
    "state",
    "on",
    "let",
    "push",
    "clear",
    "keep",
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

const ITEMS: &str = "'const', 'type', 'tool', 'tools', 'code', 'spawn', 'model', 'state', 'invariant', 'fn', 'before', 'after' or 'on'";
pub const SESSION_END_HANDLER: &str = "session_end";
pub const CODE_TOOLS_FIELD: &str = "tools";
pub const WAIT_BUILTIN: &str = "wait";

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
            "code" => self.code().map(Item::Code),
            "tools" => self.listing().map(Item::Listing),
            "model" => self.model().map(Item::Model),
            "state" => self.state().map(Item::State),
            "invariant" => self.invariant().map(Item::Invariant),
            "fn" => self.function().map(Item::Function),
            "spawn" => self.spawn().map(Item::Spawn),
            "before" | "after" => self.tool_hook().map(Item::Hook),
            "on" => self.handler_or_event(),
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
        if matches!(self.peek().token, Token::Text(_)) {
            let mut values = vec![self.text_value("a text value")?];
            while self.is_symbol(Symbol::Bar) {
                self.advance();
                values.push(self.text_value("a text value")?);
            }
            return Ok(TypeDef {
                name,
                body: TypeBody::Values(values),
            });
        }
        let mut variants = vec![self.variant()?];
        while self.is_symbol(Symbol::Bar) {
            self.advance();
            variants.push(self.variant()?);
        }
        Ok(TypeDef {
            name,
            body: TypeBody::Variants(variants),
        })
    }

    fn text_value(&mut self, what: &str) -> Parsed<Name> {
        let at = self.at();
        let Token::Text(text) = self.peek().token.clone() else {
            return Err(self.unexpected(what));
        };
        self.advance();
        Ok(Name { text, at })
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
        let (params, defaults) = self.tool_params()?;
        self.symbol(Symbol::Arrow)?;
        let returns = self.type_expr()?;
        let attaches = if self.is_word("or") {
            self.advance();
            Some(self.type_expr()?)
        } else {
            None
        };
        let effect = self.effect_class()?;
        let finds = if self.is_word("finds") {
            self.advance();
            Some(self.name("the listing a search finds tools of")?)
        } else {
            None
        };
        Ok(Tool {
            name,
            params,
            defaults,
            returns,
            attaches,
            effect,
            finds,
        })
    }

    fn effect_class(&mut self) -> Parsed<EffectClass> {
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
        Ok(effect)
    }

    fn listing(&mut self) -> Parsed<Listing> {
        self.word("tools")?;
        self.word("from")?;
        self.word("listing")?;
        self.symbol(Symbol::LeftParen)?;
        let name = self.name("a listing name")?;
        self.symbol(Symbol::Comma)?;
        let bound = self.bound("the tools one listing may hold")?;
        self.symbol(Symbol::RightParen)?;
        let (params, defaults) = self.tool_params()?;
        let effect = self.effect_class()?;
        Ok(Listing {
            name,
            bound,
            params,
            defaults,
            effect,
        })
    }

    fn code(&mut self) -> Parsed<Code> {
        self.word("code")?;
        let name = self.name("a code tool name")?;
        let open = self.symbol(Symbol::LeftBrace)?;
        let mut tools = Vec::new();
        let mut fields = Vec::new();
        while !self.is_symbol(Symbol::RightBrace) {
            if self.peek().token == Token::End {
                return Err(Diagnostic::new(
                    open,
                    "code block is not closed before the end of the file",
                ));
            }
            let key = self.name(&format!("a field of code '{}'", name.text))?;
            if key.text == CODE_TOOLS_FIELD {
                tools.push(self.name("a nested tool name")?);
                while self.is_symbol(Symbol::Comma) {
                    self.advance();
                    tools.push(self.name("a nested tool name")?);
                }
                continue;
            }
            let value = self.bound(&format!("field '{}' of code '{}'", key.text, name.text))?;
            fields.push(CodeField { key, value });
        }
        self.advance();
        Ok(Code {
            name,
            tools,
            fields,
        })
    }

    fn model(&mut self) -> Parsed<Model> {
        self.word("model")?;
        let name = self.name("a model name")?;
        self.word("calls")?;
        let calls = self.bound("the calls in one model reply")?;
        let reply = if self.is_word("reply") {
            self.advance();
            Some(self.name("the model's reply type")?)
        } else {
            None
        };
        let window = if self.is_word("window") {
            self.advance();
            Some(self.bound("the model's context window in tokens")?)
        } else {
            None
        };
        let mut modes = Vec::new();
        if self.is_word("modes") {
            self.advance();
            modes.push(self.name("a model call mode")?);
            while self.is_symbol(Symbol::Comma) {
                self.advance();
                modes.push(self.name("a model call mode")?);
            }
        }
        Ok(Model {
            name,
            calls,
            reply,
            modes,
            window,
        })
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

    fn function(&mut self) -> Parsed<Function> {
        self.word("fn")?;
        let name = self.name("a fn name")?;
        let params = self.params()?;
        let body = self.block()?;
        Ok(Function { name, params, body })
    }

    fn spawn(&mut self) -> Parsed<Spawn> {
        self.word("spawn")?;
        let name = self.name("a spawn tool name")?;
        self.word("runs")?;
        let handler = self.name("the handler the child session runs")?;
        self.word("from")?;
        let file = self.text_value("the child harness's file, as text")?;
        let sessions = if self.is_word("sessions") {
            self.advance();
            self.word("at")?;
            self.word("most")?;
            Some(self.bound("the most sessions one spawn starts")?)
        } else {
            None
        };
        Ok(Spawn {
            name,
            handler,
            file,
            sessions,
        })
    }

    fn tool_hook(&mut self) -> Parsed<Hook> {
        let at = self.at();
        let kind = if self.is_word("before") {
            self.word("before")?;
            HookKind::BeforeTool
        } else {
            self.word("after")?;
            HookKind::AfterTool
        };
        self.word("tool")?;
        let params = self.params()?;
        let body = self.block()?;
        Ok(Hook {
            kind,
            at,
            params,
            body,
        })
    }

    fn lifecycle(&mut self) -> Parsed<Option<Item>> {
        let at = self.at();
        let Token::Name(first) = self.peek().token.clone() else {
            return Ok(None);
        };
        let second = match self.peek_second() {
            Token::Name(word) => Some(word.clone()),
            Token::Symbol(Symbol::LeftBrace) => None,
            _ => return Ok(None),
        };
        if first == "session" && second.as_deref() == Some("end") {
            self.word("session")?;
            self.word("end")?;
            let name = Name {
                text: SESSION_END_HANDLER.to_owned(),
                at,
            };
            let params = self.params()?;
            let mut handler = self.handler(name, params)?;
            handler.ends_session = true;
            return Ok(Some(Item::Handler(handler)));
        }
        let Some(kind) = HookKind::ALL.into_iter().find(|kind| {
            let (one, two) = kind.words();
            !kind.denies() && one == first && second.as_deref() == (!two.is_empty()).then_some(two)
        }) else {
            return Ok(None);
        };
        match kind {
            HookKind::SessionStart => {
                self.word("session")?;
                self.word("start")?;
            }
            HookKind::PromptSubmit => {
                self.word("prompt")?;
                self.word("submit")?;
            }
            HookKind::PreCompact => {
                self.word("pre")?;
                self.word("compact")?;
            }
            HookKind::PostCompact => {
                self.word("post")?;
                self.word("compact")?;
            }
            HookKind::Stop => {
                self.word("stop")?;
            }
            HookKind::SubagentStart => {
                self.word("subagent")?;
                self.word("start")?;
            }
            HookKind::SubagentStop => {
                self.word("subagent")?;
                self.word("stop")?;
            }
            HookKind::BeforeTool | HookKind::AfterTool => {
                panic!("a tool hook starts with before or after, not on")
            }
        }
        let params = if self.is_symbol(Symbol::LeftParen) {
            self.params()?
        } else {
            Vec::new()
        };
        let body = self.block()?;
        Ok(Some(Item::Hook(Hook {
            kind,
            at,
            params,
            body,
        })))
    }

    fn handler_or_event(&mut self) -> Parsed<Item> {
        self.word("on")?;
        if self.is_word("side") && matches!(self.peek_second(), Token::Name(_)) {
            self.word("side")?;
            let name = self.name("a side handler name")?;
            let params = self.params()?;
            let mut handler = self.handler(name, params)?;
            handler.side = true;
            return Ok(Item::Handler(handler));
        }
        if let Some(item) = self.lifecycle()? {
            return Ok(item);
        }
        let name = self.name("a handler or event name")?;
        if !self.is_symbol(Symbol::LeftParen) {
            return Ok(Item::Event(Event {
                name,
                params: Vec::new(),
                bound: None,
            }));
        }
        let params = self.params()?;
        if self.is_symbol(Symbol::Arrow) {
            return self.handler(name, params).map(Item::Handler);
        }
        if !self.is_word("bound") {
            return Err(
                self.unexpected("'->' and a result type for a handler, or 'bound' for an event")
            );
        }
        self.advance();
        let bound = self.bound("the events one turn records")?;
        Ok(Item::Event(Event {
            name,
            params,
            bound: Some(bound),
        }))
    }

    fn handler(&mut self, name: Name, params: Vec<Param>) -> Parsed<Handler> {
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
            ends_session: false,
            side: false,
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

    fn tool_params(&mut self) -> Parsed<(Vec<Param>, Vec<Option<Expr>>)> {
        self.symbol(Symbol::LeftParen)?;
        let mut params = Vec::new();
        let mut defaults = Vec::new();
        if self.is_symbol(Symbol::RightParen) {
            self.advance();
            return Ok((params, defaults));
        }
        loop {
            let name = self.name("a parameter name")?;
            self.symbol(Symbol::Colon)?;
            let ty = self.type_expr()?;
            params.push(Param { name, ty });
            defaults.push(if self.is_symbol(Symbol::Assign) {
                self.advance();
                Some(self.expr()?)
            } else {
                None
            });
            if self.is_symbol(Symbol::Comma) {
                self.advance();
                continue;
            }
            self.symbol(Symbol::RightParen)?;
            return Ok((params, defaults));
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
        } else if self.is_word("blob") && self.peek_second() == &Token::Symbol(Symbol::Less) {
            self.advance();
            self.symbol(Symbol::Less)?;
            let bytes = self.bound("the bytes a blob may hold")?;
            self.symbol(Symbol::Greater)?;
            TypeExpr::Blob { at, bytes }
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
                let ty = if self.is_symbol(Symbol::Colon) {
                    self.advance();
                    Some(self.type_expr()?)
                } else {
                    None
                };
                self.symbol(Symbol::Assign)?;
                let value = self.expr()?;
                Ok(Stmt::Let { name, ty, value })
            }
            "push" => {
                self.advance();
                let list = self.name("a list to push onto")?;
                let value = self.expr()?;
                Ok(Stmt::Push { at, list, value })
            }
            "clear" => {
                self.advance();
                let list = self.name("a list to clear")?;
                Ok(Stmt::Clear { at, list })
            }
            "keep" => {
                self.advance();
                let list = self.name("a list to keep the end of")?;
                if self.is_word("blobs") {
                    self.advance();
                    self.word("last")?;
                    let count = self.bound("the blobs a keep leaves")?;
                    return Ok(Stmt::KeepBlobs { at, list, count });
                }
                self.word("last")?;
                let count = self.expr()?;
                let first = if self.is_word("after") {
                    self.advance();
                    Some(self.expr()?)
                } else {
                    None
                };
                Ok(Stmt::Keep {
                    at,
                    list,
                    count,
                    first,
                })
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
                let parallel = self.is_word("parallel");
                if parallel {
                    self.advance();
                }
                let body = self.block()?;
                Ok(Stmt::For {
                    at,
                    name,
                    list,
                    parallel,
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
                if self.is_symbol(Symbol::LeftParen) {
                    self.advance();
                    let args = self.arguments()?;
                    return Ok(Stmt::Call { callee: name, args });
                }
                if !self.is_symbol(Symbol::Assign) {
                    return Err(self.unexpected("'=' or '(' after a name that starts a statement"));
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
            let pattern = self.pattern()?;
            self.symbol(Symbol::FatArrow)?;
            let body = self.block()?;
            arms.push(Arm { pattern, body });
            if self.is_symbol(Symbol::Comma) {
                self.advance();
            }
        }
        self.advance();
        Ok(Stmt::Match { at, subject, arms })
    }

    fn pattern(&mut self) -> Parsed<Pattern> {
        if matches!(self.peek().token, Token::Text(_)) {
            return Ok(Pattern::Value(self.text_value("a value pattern")?));
        }
        let name = self.name("a variant pattern")?;
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
        Ok(Pattern::Variant { name, bindings })
    }

    pub fn expr(&mut self) -> Parsed<Expr> {
        self.enter()?;
        let parsed = self.with();
        self.leave();
        parsed
    }

    fn with(&mut self) -> Parsed<Expr> {
        let list = self.or()?;
        if !self.is_word("with") {
            return Ok(list);
        }
        let at = self.advance().at;
        let entry = self.or()?;
        Ok(Expr::With {
            at,
            list: Box::new(list),
            entry: Box::new(entry),
        })
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
        if self.is_word("take") && matches!(self.peek_second(), Token::Name(_)) {
            self.advance();
            let events = self.name("the events to take")?;
            return Ok(Expr::Take { at, events });
        }
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
                let mut args = self.arguments()?;
                if callee.text == WAIT_BUILTIN && self.is_word("at") {
                    self.advance();
                    self.word("most")?;
                    args.push(Expr::Name(self.bound("the most a wait sleeps")?));
                }
                let mode = if self.is_word("mode") && matches!(self.peek_second(), Token::Name(_)) {
                    self.advance();
                    Some(self.name("a model call mode")?)
                } else {
                    None
                };
                Ok(Expr::Call { callee, args, mode })
            }
            _ => Err(self.unexpected("an expression")),
        }
    }

    fn arguments(&mut self) -> Parsed<Vec<Expr>> {
        let mut args = Vec::new();
        if self.is_symbol(Symbol::RightParen) {
            self.advance();
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.is_symbol(Symbol::Comma) {
                self.advance();
                continue;
            }
            self.symbol(Symbol::RightParen)?;
            return Ok(args);
        }
    }
}
