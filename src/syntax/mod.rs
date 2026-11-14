pub mod ast;
pub mod lexer;
pub mod parser;

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

impl fmt::Display for Position {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.line, self.column)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub at: Position,
    pub message: String,
}

impl Diagnostic {
    pub fn new(at: Position, message: impl Into<String>) -> Self {
        Self {
            at,
            message: message.into(),
        }
    }

    pub fn render(&self, file: &str) -> String {
        format!("{file}:{}: error: {}", self.at, self.message)
    }
}

pub fn parse(source: &str) -> Result<ast::Program, Diagnostic> {
    let lexemes = lexer::lex(source)?;
    parser::Parser::new(&lexemes).program()
}
