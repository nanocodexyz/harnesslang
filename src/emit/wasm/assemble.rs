use std::collections::BTreeMap;

use super::encode::{
    Data, Export, ExportKind, FuncType, Function, Global, Import, Module, OP_END, OP_I32_CONST, OP_I64_CONST, ValType, count, index_u32,
    signed, unsigned,
};

const NESTING_MAX: usize = 4096;
const OP_BLOCK: u8 = 0x02;
const OP_LOOP: u8 = 0x03;
const OP_IF: u8 = 0x04;
const OP_ELSE: u8 = 0x05;
const OP_BR: u8 = 0x0c;
const OP_BR_IF: u8 = 0x0d;
const OP_BR_TABLE: u8 = 0x0e;
const OP_CALL: u8 = 0x10;
const OP_LOCAL_GET: u8 = 0x20;
const OP_LOCAL_SET: u8 = 0x21;
const OP_LOCAL_TEE: u8 = 0x22;
const OP_GLOBAL_GET: u8 = 0x23;
const OP_GLOBAL_SET: u8 = 0x24;
const OP_MEMORY_SIZE: u8 = 0x3f;
const OP_MEMORY_GROW: u8 = 0x40;
const OP_PREFIX_MISC: u8 = 0xfc;
const MISC_MEMORY_COPY: u64 = 10;
const MISC_MEMORY_FILL: u64 = 11;
const MEMORY_ZERO: u8 = 0;
const BLOCK_EMPTY: u8 = 0x40;
const HEX_RADIX: u32 = 16;
const DECIMAL_RADIX: u32 = 10;

#[derive(Clone, Debug)]
enum Sexp {
    List(Vec<Sexp>, usize),
    Atom(String, usize),
    Text(Vec<u8>, usize),
}

impl Sexp {
    fn line(&self) -> usize {
        match self {
            Self::List(_, line) | Self::Atom(_, line) | Self::Text(_, line) => *line,
        }
    }

    fn atom(&self) -> Option<&str> {
        match self {
            Self::Atom(word, _) => Some(word),
            _ => None,
        }
    }

    fn head(&self) -> Option<&str> {
        match self {
            Self::List(items, _) => items.first().and_then(Sexp::atom),
            _ => None,
        }
    }
}

fn fail<T>(line: usize, message: impl std::fmt::Display) -> Result<T, String> {
    Err(format!("wat line {line}: {message}"))
}

fn tokenize(text: &str) -> Result<Vec<Sexp>, String> {
    let bytes = text.as_bytes();
    let mut stack: Vec<(Vec<Sexp>, usize)> = vec![(Vec::new(), 1)];
    let mut line = 1;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        match byte {
            b'\n' => {
                line += 1;
                at += 1;
            }
            b' ' | b'\t' | b'\r' => at += 1,
            b'(' => {
                if stack.len() > NESTING_MAX {
                    return fail(line, format!("nesting deeper than nesting_max ({NESTING_MAX})"));
                }
                stack.push((Vec::new(), line));
                at += 1;
            }
            b')' => {
                let (items, opened) = stack.pop().expect("the stack holds the top level");
                let Some(parent) = stack.last_mut() else {
                    return fail(line, "unbalanced ')'");
                };
                parent.0.push(Sexp::List(items, opened));
                at += 1;
            }
            b'"' => {
                let (value, next) = string(bytes, at + 1, line)?;
                stack.last_mut().expect("the stack holds the top level").0.push(Sexp::Text(value, line));
                at = next;
            }
            _ => {
                let start = at;
                while at < bytes.len() && !matches!(bytes[at], b' ' | b'\t' | b'\r' | b'\n' | b'(' | b')' | b'"') {
                    at += 1;
                }
                let word = std::str::from_utf8(&bytes[start..at]).expect("a word of utf-8 text is utf-8");
                stack.last_mut().expect("the stack holds the top level").0.push(Sexp::Atom(word.to_owned(), line));
            }
        }
    }
    if stack.len() != 1 {
        return fail(line, "unbalanced '('");
    }
    Ok(stack.pop().expect("the top level").0)
}

fn string(bytes: &[u8], mut at: usize, line: usize) -> Result<(Vec<u8>, usize), String> {
    let mut value = Vec::new();
    loop {
        let Some(&byte) = bytes.get(at) else {
            return fail(line, "unterminated string");
        };
        match byte {
            b'"' => return Ok((value, at + 1)),
            b'\\' => {
                let escape = bytes.get(at + 1).copied();
                let (decoded, width) = match escape {
                    Some(b'n') => (b'\n', 2),
                    Some(b't') => (b'\t', 2),
                    Some(b'\\') => (b'\\', 2),
                    Some(b'"') => (b'"', 2),
                    Some(b'\'') => (b'\'', 2),
                    _ => {
                        let digits = bytes.get(at + 1..at + 3).and_then(|pair| std::str::from_utf8(pair).ok());
                        match digits.and_then(|pair| u8::from_str_radix(pair, HEX_RADIX).ok()) {
                            Some(decoded) => (decoded, 3),
                            None => return fail(line, "bad string escape"),
                        }
                    }
                };
                value.push(decoded);
                at += width;
            }
            _ => {
                value.push(byte);
                at += 1;
            }
        }
    }
}

pub fn number(word: &str) -> Option<i128> {
    let (negative, digits) = match word.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, word),
    };
    let cleaned: String = digits.chars().filter(|character| *character != '_').collect();
    let magnitude = match cleaned.strip_prefix("0x") {
        Some(hex) => i128::from_str_radix(hex, HEX_RADIX).ok()?,
        None => i128::from_str_radix(&cleaned, DECIMAL_RADIX).ok()?,
    };
    Some(if negative { -magnitude } else { magnitude })
}

#[derive(Clone, Copy)]
enum Immediate {
    None,
    Memory(u32),
}

fn simple_op(word: &str) -> Option<(u8, Immediate)> {
    let plain = |code: u8| Some((code, Immediate::None));
    let memory = |code: u8, align: u32| Some((code, Immediate::Memory(align)));
    match word {
        "unreachable" => plain(0x00),
        "nop" => plain(0x01),
        "return" => plain(0x0f),
        "drop" => plain(0x1a),
        "select" => plain(0x1b),
        "i32.load" => memory(0x28, 2),
        "i64.load" => memory(0x29, 3),
        "i32.load8_s" => memory(0x2c, 0),
        "i32.load8_u" => memory(0x2d, 0),
        "i32.load16_u" => memory(0x2f, 1),
        "i64.load8_u" => memory(0x31, 0),
        "i64.load32_u" => memory(0x35, 2),
        "i32.store" => memory(0x36, 2),
        "i64.store" => memory(0x37, 3),
        "i32.store8" => memory(0x3a, 0),
        "i32.store16" => memory(0x3b, 1),
        "i64.store8" => memory(0x3c, 0),
        "i64.store32" => memory(0x3e, 2),
        "i32.eqz" => plain(0x45),
        "i64.eqz" => plain(0x50),
        "i32.wrap_i64" => plain(0xa7),
        "i64.extend_i32_s" => plain(0xac),
        "i64.extend_i32_u" => plain(0xad),
        _ => numeric(word).map(|code| (code, Immediate::None)),
    }
}

const COMPARISONS: [&str; 10] = ["eq", "ne", "lt_s", "lt_u", "gt_s", "gt_u", "le_s", "le_u", "ge_s", "ge_u"];
const ARITHMETIC: [&str; 18] = [
    "clz", "ctz", "popcnt", "add", "sub", "mul", "div_s", "div_u", "rem_s", "rem_u", "and", "or", "xor", "shl", "shr_s", "shr_u", "rotl",
    "rotr",
];
const I32_COMPARISON_FIRST: u8 = 0x46;
const I64_COMPARISON_FIRST: u8 = 0x51;
const I32_ARITHMETIC_FIRST: u8 = 0x67;
const I64_ARITHMETIC_FIRST: u8 = 0x79;

fn numeric(word: &str) -> Option<u8> {
    let (ty, op) = word.split_once('.')?;
    let (comparison, arithmetic) = match ty {
        "i32" => (I32_COMPARISON_FIRST, I32_ARITHMETIC_FIRST),
        "i64" => (I64_COMPARISON_FIRST, I64_ARITHMETIC_FIRST),
        _ => return None,
    };
    if let Some(index) = COMPARISONS.iter().position(|name| *name == op) {
        return Some(comparison + u8::try_from(index).expect("a comparison index fits u8"));
    }
    let index = ARITHMETIC.iter().position(|name| *name == op)?;
    Some(arithmetic + u8::try_from(index).expect("an arithmetic index fits u8"))
}

struct Names {
    functions: BTreeMap<String, u32>,
    globals: BTreeMap<String, u32>,
    symbols: BTreeMap<String, u32>,
}

impl Names {
    fn value(&self, word: &str) -> Option<i128> {
        match self.symbols.get(word) {
            Some(address) => Some(i128::from(*address)),
            None => number(word),
        }
    }
}

struct Body<'a> {
    names: &'a Names,
    locals: BTreeMap<String, u32>,
    labels: Vec<Option<String>>,
    out: Vec<u8>,
}

impl Body<'_> {
    fn index(&self, word: &Sexp, table: &BTreeMap<String, u32>, what: &str) -> Result<u32, String> {
        let line = word.line();
        let Some(text) = word.atom() else {
            return fail(line, format!("expected a {what}"));
        };
        if let Some(index) = table.get(text) {
            return Ok(*index);
        }
        match number(text).and_then(|value| u32::try_from(value).ok()) {
            Some(index) => Ok(index),
            None => fail(line, format!("unknown {what} {text}")),
        }
    }

    fn label(&self, word: &Sexp) -> Result<u32, String> {
        let line = word.line();
        let Some(text) = word.atom() else {
            return fail(line, "expected a label");
        };
        if let Some(depth) = self.labels.iter().rev().position(|label| label.as_deref() == Some(text)) {
            return Ok(index_u32(depth));
        }
        match number(text).and_then(|value| u32::try_from(value).ok()) {
            Some(depth) if (depth as usize) < self.labels.len() => Ok(depth),
            _ => fail(line, format!("unknown label {text}")),
        }
    }

    fn function(&self, word: &Sexp) -> Result<u32, String> {
        let line = word.line();
        match word.atom().and_then(|text| self.names.functions.get(text)) {
            Some(index) => Ok(*index),
            None => fail(line, format!("unknown function {:?}", word.atom())),
        }
    }

    fn sequence(&mut self, items: &[Sexp]) -> Result<(), String> {
        let mut at = 0;
        while at < items.len() {
            at = self.instruction(items, at)?;
        }
        Ok(())
    }

    fn instruction(&mut self, items: &[Sexp], at: usize) -> Result<usize, String> {
        match &items[at] {
            Sexp::List(list, _) => {
                self.folded(list, items[at].line())?;
                Ok(at + 1)
            }
            Sexp::Text(_, line) => fail(*line, "a string is not an instruction"),
            Sexp::Atom(word, line) => self.flat(word, *line, items, at + 1),
        }
    }

    fn block_type(&self, items: &[Sexp], mut at: usize) -> Result<(Option<String>, u8, usize), String> {
        let mut label = None;
        if let Some(word) = items.get(at).and_then(Sexp::atom)
            && word.starts_with('$')
        {
            label = Some(word.to_owned());
            at += 1;
        }
        let mut code = BLOCK_EMPTY;
        if let Some(Sexp::List(list, line)) = items.get(at)
            && list.first().and_then(Sexp::atom) == Some("result")
        {
            let ty = list.get(1).and_then(Sexp::atom).and_then(ValType::parse);
            match (ty, list.len()) {
                (Some(ty), 2) => code = ty.code(),
                _ => return fail(*line, "a block has one result type"),
            }
            at += 1;
        }
        Ok((label, code, at))
    }

    fn flat(&mut self, word: &str, line: usize, items: &[Sexp], at: usize) -> Result<usize, String> {
        match word {
            "block" | "loop" | "if" => {
                let (label, code, next) = self.block_type(items, at)?;
                self.open(word, label, code)?;
                Ok(next)
            }
            "else" => {
                self.out.push(OP_ELSE);
                Ok(at)
            }
            "end" => {
                if self.labels.pop().is_none() {
                    return fail(line, "end without a block");
                }
                self.out.push(OP_END);
                Ok(at)
            }
            _ => self.plain(word, line, items, at),
        }
    }

    fn open(&mut self, word: &str, label: Option<String>, code: u8) -> Result<(), String> {
        self.out.push(match word {
            "block" => OP_BLOCK,
            "loop" => OP_LOOP,
            _ => OP_IF,
        });
        self.out.push(code);
        self.labels.push(label);
        Ok(())
    }

    fn immediate<'s>(&self, items: &'s [Sexp], at: usize, line: usize, word: &str) -> Result<&'s Sexp, String> {
        match items.get(at) {
            Some(item) => Ok(item),
            None => fail(line, format!("{word} needs an immediate")),
        }
    }

    fn plain(&mut self, word: &str, line: usize, items: &[Sexp], at: usize) -> Result<usize, String> {
        let locals = &self.locals;
        let globals = &self.names.globals;
        match word {
            "local.get" | "local.set" | "local.tee" => {
                let index = self.index(self.immediate(items, at, line, word)?, locals, "local")?;
                self.out.push(match word {
                    "local.get" => OP_LOCAL_GET,
                    "local.set" => OP_LOCAL_SET,
                    _ => OP_LOCAL_TEE,
                });
                unsigned(&mut self.out, u64::from(index));
                Ok(at + 1)
            }
            "global.get" | "global.set" => {
                let index = self.index(self.immediate(items, at, line, word)?, globals, "global")?;
                self.out.push(if word == "global.get" { OP_GLOBAL_GET } else { OP_GLOBAL_SET });
                unsigned(&mut self.out, u64::from(index));
                Ok(at + 1)
            }
            "call" => {
                let index = self.function(self.immediate(items, at, line, word)?)?;
                self.out.push(OP_CALL);
                unsigned(&mut self.out, u64::from(index));
                Ok(at + 1)
            }
            "br" | "br_if" => {
                let depth = self.label(self.immediate(items, at, line, word)?)?;
                self.out.push(if word == "br" { OP_BR } else { OP_BR_IF });
                unsigned(&mut self.out, u64::from(depth));
                Ok(at + 1)
            }
            "br_table" => self.br_table(items, at, line),
            "i32.const" | "i64.const" => self.constant(word, line, self.immediate(items, at, line, word)?).map(|()| at + 1),
            "memory.size" | "memory.grow" => {
                self.out.push(if word == "memory.size" { OP_MEMORY_SIZE } else { OP_MEMORY_GROW });
                self.out.push(MEMORY_ZERO);
                Ok(at)
            }
            "memory.copy" | "memory.fill" => {
                self.out.push(OP_PREFIX_MISC);
                let copy = word == "memory.copy";
                unsigned(&mut self.out, if copy { MISC_MEMORY_COPY } else { MISC_MEMORY_FILL });
                self.out.push(MEMORY_ZERO);
                if copy {
                    self.out.push(MEMORY_ZERO);
                }
                Ok(at)
            }
            _ => self.simple(word, line, items, at),
        }
    }

    fn br_table(&mut self, items: &[Sexp], mut at: usize, line: usize) -> Result<usize, String> {
        let mut depths = Vec::new();
        while let Some(item) = items.get(at)
            && item.atom().is_some_and(|word| word.starts_with('$') || number(word).is_some())
        {
            depths.push(self.label(item)?);
            at += 1;
        }
        let Some(default) = depths.pop() else {
            return fail(line, "br_table needs a default label");
        };
        self.out.push(OP_BR_TABLE);
        count(&mut self.out, depths.len());
        for depth in depths {
            unsigned(&mut self.out, u64::from(depth));
        }
        unsigned(&mut self.out, u64::from(default));
        Ok(at)
    }

    fn constant(&mut self, word: &str, line: usize, item: &Sexp) -> Result<(), String> {
        let Some(value) = item.atom().and_then(|text| self.names.value(text)) else {
            return fail(line, format!("{word} needs a number"));
        };
        if word == "i32.const" {
            let Some(wrapped) = i32::try_from(value).ok().or_else(|| u32::try_from(value).ok().map(|unsigned| unsigned as i32)) else {
                return fail(line, format!("{value} does not fit i32"));
            };
            self.out.push(OP_I32_CONST);
            signed(&mut self.out, i64::from(wrapped));
        } else {
            let Some(wrapped) = i64::try_from(value).ok().or_else(|| u64::try_from(value).ok().map(|unsigned| unsigned as i64)) else {
                return fail(line, format!("{value} does not fit i64"));
            };
            self.out.push(OP_I64_CONST);
            signed(&mut self.out, wrapped);
        }
        Ok(())
    }

    fn simple(&mut self, word: &str, line: usize, items: &[Sexp], mut at: usize) -> Result<usize, String> {
        let Some((code, immediate)) = simple_op(word) else {
            return fail(line, format!("unknown instruction {word}"));
        };
        self.out.push(code);
        if let Immediate::Memory(natural) = immediate {
            let mut offset = 0;
            let mut align = natural;
            while let Some(argument) = items.get(at).and_then(Sexp::atom) {
                if let Some(value) = argument.strip_prefix("offset=") {
                    offset = number(value).and_then(|value| u32::try_from(value).ok()).ok_or_else(|| format!("wat line {line}: bad offset"))?;
                } else if let Some(value) = argument.strip_prefix("align=") {
                    let bytes = number(value).and_then(|value| u32::try_from(value).ok()).ok_or_else(|| format!("wat line {line}: bad align"))?;
                    align = bytes.trailing_zeros();
                } else {
                    break;
                }
                at += 1;
            }
            unsigned(&mut self.out, u64::from(align));
            unsigned(&mut self.out, u64::from(offset));
        }
        Ok(at)
    }

    fn folded(&mut self, list: &[Sexp], line: usize) -> Result<(), String> {
        let Some(word) = list.first().and_then(Sexp::atom) else {
            return fail(line, "a folded instruction starts with its name");
        };
        match word {
            "block" | "loop" => {
                let (label, code, next) = self.block_type(list, 1)?;
                self.open(word, label, code)?;
                self.sequence(&list[next..])?;
                self.labels.pop();
                self.out.push(OP_END);
                Ok(())
            }
            "if" => self.folded_if(list, line),
            "then" | "else" => fail(line, format!("{word} outside an if")),
            _ => {
                let mut at = 1;
                while at < list.len() && !matches!(list[at], Sexp::List(..)) {
                    at += 1;
                }
                for operand in &list[at..] {
                    let Sexp::List(inner, inner_line) = operand else {
                        return fail(operand.line(), "an operand after an immediate is folded");
                    };
                    self.folded(inner, *inner_line)?;
                }
                let consumed = self.flat(word, line, &list[..at], 1)?;
                if consumed != at {
                    return fail(line, format!("{word} has extra immediates"));
                }
                Ok(())
            }
        }
    }

    fn folded_if(&mut self, list: &[Sexp], line: usize) -> Result<(), String> {
        let (label, code, mut at) = self.block_type(list, 1)?;
        while at < list.len() && !matches!(list[at].head(), Some("then")) {
            let Sexp::List(inner, inner_line) = &list[at] else {
                return fail(line, "an if's condition is folded");
            };
            self.folded(inner, *inner_line)?;
            at += 1;
        }
        let Some(Sexp::List(then, _)) = list.get(at) else {
            return fail(line, "an if needs (then ...)");
        };
        self.open("if", label, code)?;
        self.sequence(&then[1..])?;
        if let Some(Sexp::List(otherwise, _)) = list.get(at + 1) {
            if otherwise.first().and_then(Sexp::atom) != Some("else") {
                return fail(line, "an if's second arm is (else ...)");
            }
            self.out.push(OP_ELSE);
            self.sequence(&otherwise[1..])?;
        }
        if list.len() > at + 2 {
            return fail(line, "an if has at most then and else");
        }
        self.labels.pop();
        self.out.push(OP_END);
        Ok(())
    }
}

struct Header {
    name: Option<String>,
    export: Option<String>,
    params: Vec<(Option<String>, ValType)>,
    results: Vec<ValType>,
    locals: Vec<(Option<String>, ValType)>,
    body_at: usize,
}

fn typed_fields(list: &[Sexp], line: usize, into: &mut Vec<(Option<String>, ValType)>) -> Result<(), String> {
    let words: Vec<&str> = list[1..].iter().filter_map(Sexp::atom).collect();
    if words.len() != list.len() - 1 {
        return fail(line, "a param, result or local lists types");
    }
    if let Some(name) = words.first().filter(|word| word.starts_with('$')) {
        let ty = words.get(1).and_then(|word| ValType::parse(word));
        match (ty, words.len()) {
            (Some(ty), 2) => into.push((Some((*name).to_owned()), ty)),
            _ => return fail(line, "a named param or local has one type"),
        }
        return Ok(());
    }
    for word in words {
        match ValType::parse(word) {
            Some(ty) => into.push((None, ty)),
            None => return fail(line, format!("unknown type {word}")),
        }
    }
    Ok(())
}

fn header(items: &[Sexp], line: usize) -> Result<Header, String> {
    let mut header = Header { name: None, export: None, params: Vec::new(), results: Vec::new(), locals: Vec::new(), body_at: items.len() };
    let mut at = 1;
    if let Some(word) = items.get(at).and_then(Sexp::atom)
        && word.starts_with('$')
    {
        header.name = Some(word.to_owned());
        at += 1;
    }
    while let Some(Sexp::List(list, inner_line)) = items.get(at) {
        match list.first().and_then(Sexp::atom) {
            Some("export") => match list.get(1) {
                Some(Sexp::Text(name, _)) => header.export = Some(String::from_utf8_lossy(name).into_owned()),
                _ => return fail(*inner_line, "an export names itself"),
            },
            Some("param") => typed_fields(list, *inner_line, &mut header.params)?,
            Some("result") => {
                let mut results = Vec::new();
                typed_fields(list, *inner_line, &mut results)?;
                header.results.extend(results.into_iter().map(|(_, ty)| ty));
            }
            Some("local") => typed_fields(list, *inner_line, &mut header.locals)?,
            _ => break,
        }
        at += 1;
    }
    header.body_at = at;
    if header.name.is_none() {
        return fail(line, "a function is named");
    }
    Ok(header)
}

pub fn assemble(text: &str) -> Result<Module, String> {
    let forms = tokenize(text)?;
    let mut module = Module::default();
    let mut names = Names { functions: BTreeMap::new(), globals: BTreeMap::new(), symbols: BTreeMap::new() };
    let mut bodies = Vec::new();
    layout(&forms, &mut module, &mut names)?;
    for form in &forms {
        if form.head() == Some("import") {
            import(form, &mut module, &mut names)?;
        }
    }
    let imported = index_u32(module.imports.len());
    for form in &forms {
        let Sexp::List(items, line) = form else {
            return fail(form.line(), "a top-level form is a list");
        };
        match form.head() {
            Some("import" | "text" | "words") => {}
            Some("func") => {
                let parsed = header(items, *line)?;
                let ty = FuncType { params: parsed.params.iter().map(|(_, ty)| *ty).collect(), results: parsed.results.clone() };
                let index = imported + index_u32(bodies.len());
                let name = parsed.name.clone().expect("a header is named");
                let ty_index = module.type_index(ty);
                if names.functions.insert(name, index).is_some() {
                    return fail(*line, "a function is named twice");
                }
                if let Some(export) = &parsed.export {
                    module.exports.push(Export { name: export.clone(), kind: ExportKind::Function, index });
                }
                bodies.push((parsed, items, ty_index));
            }
            Some("global") => global(items, *line, &mut module, &mut names)?,
            Some("memory") => memory(items, *line, &mut module)?,
            Some("data") => data(items, *line, &mut module)?,
            _ => return fail(*line, "unknown top-level form"),
        }
    }
    for (parsed, items, ty) in bodies {
        module.functions.push(function(&names, &parsed, items, ty)?);
    }
    Ok(module)
}

fn function(names: &Names, parsed: &Header, items: &[Sexp], ty: u32) -> Result<Function, String> {
    let mut locals = BTreeMap::new();
    for (position, (name, _)) in parsed.params.iter().chain(&parsed.locals).enumerate() {
        if let Some(name) = name {
            locals.insert(name.clone(), index_u32(position));
        }
    }
    let mut body = Body { names, locals, labels: Vec::new(), out: Vec::new() };
    body.sequence(&items[parsed.body_at..])?;
    if !body.labels.is_empty() {
        return fail(items[0].line(), "a block is not ended");
    }
    body.out.push(OP_END);
    Ok(Function { ty, locals: parsed.locals.iter().map(|(_, ty)| *ty).collect(), body: body.out })
}

fn import(form: &Sexp, module: &mut Module, names: &mut Names) -> Result<(), String> {
    let Sexp::List(items, line) = form else {
        return fail(form.line(), "an import is a list");
    };
    let (Some(Sexp::Text(from, _)), Some(Sexp::Text(name, _)), Some(Sexp::List(signature, inner_line))) = (items.get(1), items.get(2), items.get(3)) else {
        return fail(*line, "an import is (import \"module\" \"name\" (func ...))");
    };
    let parsed = header(signature, *inner_line)?;
    let ty = FuncType { params: parsed.params.iter().map(|(_, ty)| *ty).collect(), results: parsed.results };
    let index = index_u32(module.imports.len());
    let ty_index = module.type_index(ty);
    module.imports.push(Import {
        module: String::from_utf8_lossy(from).into_owned(),
        name: String::from_utf8_lossy(name).into_owned(),
        ty: ty_index,
    });
    names.functions.insert(parsed.name.expect("a header is named"), index);
    Ok(())
}

fn global(items: &[Sexp], line: usize, module: &mut Module, names: &mut Names) -> Result<(), String> {
    let Some(name) = items.get(1).and_then(Sexp::atom) else {
        return fail(line, "a global is named");
    };
    let (ty, mutable) = match items.get(2) {
        Some(Sexp::Atom(word, _)) => (ValType::parse(word), false),
        Some(Sexp::List(list, _)) if list.first().and_then(Sexp::atom) == Some("mut") => (list.get(1).and_then(Sexp::atom).and_then(ValType::parse), true),
        _ => (None, false),
    };
    let Some(ty) = ty else {
        return fail(line, "a global has a type");
    };
    let init = match items.get(3) {
        Some(Sexp::List(list, _)) if list.len() == 2 => list[1].atom().and_then(|word| names.value(word)),
        _ => None,
    };
    let Some(init) = init.and_then(|value| i64::try_from(value).ok().or_else(|| u64::try_from(value).ok().map(|unsigned| unsigned as i64))) else {
        return fail(line, "a global starts at a constant");
    };
    let index = index_u32(module.globals.len());
    if names.globals.insert(name.to_owned(), index).is_some() {
        return fail(line, "a global is named twice");
    }
    module.globals.push(Global { ty, mutable, init });
    Ok(())
}

fn memory(items: &[Sexp], line: usize, module: &mut Module) -> Result<(), String> {
    for item in &items[1..] {
        match item {
            Sexp::List(list, _) if list.first().and_then(Sexp::atom) == Some("export") => {
                let Some(Sexp::Text(name, _)) = list.get(1) else {
                    return fail(line, "an export names itself");
                };
                module.exports.push(Export { name: String::from_utf8_lossy(name).into_owned(), kind: ExportKind::Memory, index: 0 });
            }
            Sexp::Atom(word, _) => match number(word).and_then(|pages| u32::try_from(pages).ok()) {
                Some(pages) => module.memory_pages = Some(pages),
                None => return fail(line, "a memory has a page count"),
            },
            _ => return fail(line, "unknown memory field"),
        }
    }
    if module.memory_pages.is_none() {
        return fail(line, "a memory has a page count");
    }
    Ok(())
}

fn data(items: &[Sexp], line: usize, module: &mut Module) -> Result<(), String> {
    let offset = match items.get(1) {
        Some(Sexp::List(list, _)) if list.first().and_then(Sexp::atom) == Some("i32.const") && list.len() == 2 => {
            list[1].atom().and_then(number).and_then(|value| u32::try_from(value).ok())
        }
        _ => None,
    };
    let Some(offset) = offset else {
        return fail(line, "a data segment starts at (i32.const N)");
    };
    let mut bytes = Vec::new();
    for item in &items[2..] {
        let Sexp::Text(text, _) = item else {
            return fail(line, "a data segment holds strings");
        };
        bytes.extend_from_slice(text);
    }
    module.data.push(Data { offset, bytes });
    Ok(())
}

pub const STATIC_BASE_BYTES: u32 = 1024;
pub const STATIC_END: &str = "$static_end";
const WORD_BYTES: u32 = 4;
const STATIC_END_ALIGN_BYTES: u32 = 8;

fn align(value: u32, to: u32) -> u32 {
    value.div_ceil(to) * to
}

fn layout(forms: &[Sexp], module: &mut Module, names: &mut Names) -> Result<(), String> {
    let mut address = STATIC_BASE_BYTES;
    let mut placed = Vec::new();
    for form in forms {
        let Sexp::List(items, line) = form else {
            continue;
        };
        let kind = form.head();
        if !matches!(kind, Some("text" | "words")) {
            continue;
        }
        let Some(name) = items.get(1).and_then(Sexp::atom).filter(|word| word.starts_with('$')) else {
            return fail(*line, "a text or words form is named");
        };
        let size = if kind == Some("text") {
            let mut bytes = 0;
            for item in &items[2..] {
                let Sexp::Text(text, _) = item else {
                    return fail(*line, "a text holds strings");
                };
                bytes += index_u32(text.len());
            }
            WORD_BYTES + bytes
        } else {
            WORD_BYTES * index_u32(items.len() - 2)
        };
        if names.symbols.insert(name.to_owned(), address).is_some() {
            return fail(*line, format!("{name} is placed twice"));
        }
        placed.push((items, *line, kind == Some("text")));
        address = align(address + size, WORD_BYTES);
    }
    let end = align(address, STATIC_END_ALIGN_BYTES);
    names.symbols.insert(STATIC_END.to_owned(), end);
    let mut bytes = Vec::with_capacity((end - STATIC_BASE_BYTES) as usize);
    for (items, line, is_text) in placed {
        if is_text {
            let length: usize = items[2..].iter().map(|item| if let Sexp::Text(text, _) = item { text.len() } else { 0 }).sum();
            bytes.extend_from_slice(&index_u32(length).to_le_bytes());
            for item in &items[2..] {
                if let Sexp::Text(text, _) = item {
                    bytes.extend_from_slice(text);
                }
            }
        } else {
            for item in &items[2..] {
                let value = item.atom().and_then(|word| names.value(word)).and_then(|value| u32::try_from(value).ok().or_else(|| i32::try_from(value).ok().map(|signed| signed as u32)));
                let Some(value) = value else {
                    return fail(line, "a word is a number or a placed name");
                };
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        while bytes.len() % WORD_BYTES as usize != 0 {
            bytes.push(0);
        }
    }
    bytes.resize((end - STATIC_BASE_BYTES) as usize, 0);
    module.data.push(Data { offset: STATIC_BASE_BYTES, bytes });
    Ok(())
}
