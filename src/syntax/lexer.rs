use crate::syntax::{Diagnostic, Position};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symbol {
    LeftBrace,
    RightBrace,
    LeftParen,
    RightParen,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Comma,
    Colon,
    Assign,
    Equal,
    NotEqual,
    Arrow,
    FatArrow,
    Plus,
    PlusPlus,
    Minus,
    Star,
    Dot,
    Bar,
}

impl Symbol {
    pub const fn text(self) -> &'static str {
        match self {
            Self::LeftBrace => "{",
            Self::RightBrace => "}",
            Self::LeftParen => "(",
            Self::RightParen => ")",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
            Self::Comma => ",",
            Self::Colon => ":",
            Self::Assign => "=",
            Self::Equal => "==",
            Self::NotEqual => "!=",
            Self::Arrow => "->",
            Self::FatArrow => "=>",
            Self::Plus => "+",
            Self::PlusPlus => "++",
            Self::Minus => "-",
            Self::Star => "*",
            Self::Dot => ".",
            Self::Bar => "|",
        }
    }
}

const SYMBOLS: [Symbol; 21] = [
    Symbol::LessEqual,
    Symbol::GreaterEqual,
    Symbol::Equal,
    Symbol::NotEqual,
    Symbol::Arrow,
    Symbol::FatArrow,
    Symbol::PlusPlus,
    Symbol::LeftBrace,
    Symbol::RightBrace,
    Symbol::LeftParen,
    Symbol::RightParen,
    Symbol::Less,
    Symbol::Greater,
    Symbol::Comma,
    Symbol::Colon,
    Symbol::Assign,
    Symbol::Plus,
    Symbol::Minus,
    Symbol::Star,
    Symbol::Dot,
    Symbol::Bar,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Name(String),
    Number(u64),
    Text(String),
    Symbol(Symbol),
    End,
}

impl Token {
    pub fn describe(&self) -> String {
        match self {
            Self::Name(name) => format!("'{name}'"),
            Self::Number(value) => format!("number {value}"),
            Self::Text(_) => "text literal".to_owned(),
            Self::Symbol(symbol) => format!("'{}'", symbol.text()),
            Self::End => "end of file".to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Lexeme {
    pub token: Token,
    pub at: Position,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
    line: u32,
    column: u32,
}

impl Cursor<'_> {
    fn peek(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.offset + ahead).copied()
    }

    fn at(&self) -> Position {
        Position {
            line: self.line,
            column: self.column,
        }
    }

    fn bump(&mut self) {
        let byte = self.bytes[self.offset];
        self.offset += 1;
        if byte == b'\n' {
            self.line += 1;
            self.column = 1;
        } else if byte & 0xc0 != 0x80 {
            self.column += 1;
        }
    }
}

pub fn lex(source: &str) -> Result<Vec<Lexeme>, Diagnostic> {
    let mut cursor = Cursor {
        bytes: source.as_bytes(),
        offset: 0,
        line: 1,
        column: 1,
    };
    let mut lexemes = Vec::new();
    while let Some(byte) = cursor.peek(0) {
        assert!(
            lexemes.len() <= source.len(),
            "a source yields at most one token per byte"
        );
        let at = cursor.at();
        if byte == b' ' || byte == b'\t' || byte == b'\n' || byte == b'\r' {
            cursor.bump();
            continue;
        }
        let token = if byte.is_ascii_alphabetic() || byte == b'_' {
            name(&mut cursor)
        } else if byte.is_ascii_digit() {
            number(&mut cursor, at)?
        } else if byte == b'"' {
            text(&mut cursor, at)?
        } else {
            symbol(&mut cursor, at)?
        };
        lexemes.push(Lexeme { token, at });
    }
    lexemes.push(Lexeme {
        token: Token::End,
        at: cursor.at(),
    });
    Ok(lexemes)
}

fn name(cursor: &mut Cursor<'_>) -> Token {
    let start = cursor.offset;
    while cursor
        .peek(0)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        cursor.bump();
    }
    let text =
        std::str::from_utf8(&cursor.bytes[start..cursor.offset]).expect("an ascii run is utf-8");
    Token::Name(text.to_owned())
}

fn number(cursor: &mut Cursor<'_>, at: Position) -> Result<Token, Diagnostic> {
    let mut value: u64 = 0;
    while let Some(byte) = cursor.peek(0) {
        if byte == b'_' {
            cursor.bump();
            continue;
        }
        if !byte.is_ascii_digit() {
            break;
        }
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or_else(|| Diagnostic::new(at, "number does not fit in u64"))?;
        cursor.bump();
    }
    if cursor
        .peek(0)
        .is_some_and(|byte| byte.is_ascii_alphabetic())
    {
        return Err(Diagnostic::new(
            cursor.at(),
            "a number is followed by a letter",
        ));
    }
    Ok(Token::Number(value))
}

fn text(cursor: &mut Cursor<'_>, at: Position) -> Result<Token, Diagnostic> {
    cursor.bump();
    let mut bytes = Vec::new();
    loop {
        let Some(byte) = cursor.peek(0) else {
            return Err(Diagnostic::new(
                at,
                "text literal is not closed before the end of the file",
            ));
        };
        match byte {
            b'"' => {
                cursor.bump();
                break;
            }
            b'\n' => {
                return Err(Diagnostic::new(
                    at,
                    "text literal is not closed before the end of its line",
                ));
            }
            b'\\' => {
                let escape_at = cursor.at();
                cursor.bump();
                let escaped = match cursor.peek(0) {
                    Some(b'"') => b'"',
                    Some(b'\\') => b'\\',
                    Some(b'n') => b'\n',
                    _ => {
                        return Err(Diagnostic::new(
                            escape_at,
                            "unknown escape in text literal: only \\\" \\\\ and \\n are escapes",
                        ));
                    }
                };
                bytes.push(escaped);
                cursor.bump();
            }
            _ => {
                bytes.push(byte);
                cursor.bump();
            }
        }
    }
    let text =
        String::from_utf8(bytes).map_err(|_| Diagnostic::new(at, "text literal is not utf-8"))?;
    Ok(Token::Text(text))
}

fn symbol(cursor: &mut Cursor<'_>, at: Position) -> Result<Token, Diagnostic> {
    for symbol in SYMBOLS {
        let text = symbol.text().as_bytes();
        let matches = text
            .iter()
            .enumerate()
            .all(|(index, byte)| cursor.peek(index) == Some(*byte));
        if matches {
            for _ in text {
                cursor.bump();
            }
            return Ok(Token::Symbol(symbol));
        }
    }
    let rest = std::str::from_utf8(&cursor.bytes[cursor.offset..]).unwrap_or("");
    let shown = rest.chars().next().unwrap_or('?');
    Err(Diagnostic::new(
        at,
        format!("unexpected character {shown:?}"),
    ))
}
