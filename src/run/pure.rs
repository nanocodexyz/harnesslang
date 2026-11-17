use std::rc::Rc;

use crate::ir::{self, BytesOp, Expr, FunctionId, Program, Stmt, Type};
use crate::run::engine::{Reads, evaluate_with};
use crate::run::value::Value;

enum Flow {
    Next,
    Return(Value),
}

struct Frame<'p, 'r> {
    program: &'p Program,
    function: &'p ir::Handler,
    locals: Vec<Option<Value>>,
    reads: Reads<'r>,
}

pub fn call(
    program: &Program,
    function: FunctionId,
    args: Vec<Value>,
    reads: Reads<'_>,
) -> Result<Value, String> {
    let definition = &program.functions[function];
    assert_eq!(
        args.len(),
        definition.params.len(),
        "a call passes each param"
    );
    let mut frame = Frame {
        program,
        function: definition,
        locals: vec![None; definition.locals.len()],
        reads: Reads {
            cancelled: None,
            uncut: reads.uncut,
            notices: None,
        },
    };
    for (param, value) in definition.params.iter().zip(args) {
        frame.locals[*param] = Some(value);
    }
    match frame.block(&definition.body)? {
        Flow::Return(value) => Ok(value),
        Flow::Next => panic!("the checker proves a pure fn returns"),
    }
}

pub fn bytes(
    program: &Program,
    op: BytesOp,
    exprs: &[Expr],
    args: Vec<Value>,
) -> Result<Value, String> {
    Ok(match op {
        BytesOp::Get => index(&args[0], &args[1])?,
        BytesOp::Slice => {
            let buffer = args[0].as_bytes();
            let from = args[1].as_u64();
            let to = args[2].as_u64();
            if from > to || to > buffer.len() as u64 {
                return Err("index".to_owned());
            }
            Value::Bytes(Rc::new(buffer[from as usize..to as usize].to_vec()))
        }
        BytesOp::OfText => {
            let Expr::Constant(capacity) = &exprs[1] else {
                panic!("the checker passes bytes_of its capacity constant");
            };
            let capacity = &program.constants[*capacity];
            let text = args[0].as_text();
            if text.len() as u64 > capacity.value {
                return Err(format!("bound({})", capacity.name));
            }
            Value::Bytes(Rc::new(text.as_bytes().to_vec()))
        }
        BytesOp::ToText => Value::Text(
            String::from_utf8(args[0].as_bytes().to_vec()).map_err(|_| "text_of".to_owned())?,
        ),
        BytesOp::OfHex => {
            let Expr::Constant(capacity) = &exprs[1] else {
                panic!("the checker passes bytes_of_hex its capacity constant");
            };
            let capacity = &program.constants[*capacity];
            Value::Bytes(Rc::new(unhex(args[0].as_text().as_bytes(), capacity)?))
        }
        BytesOp::ToHex => Value::Text(hex(args[0].as_bytes())),
        BytesOp::Empty => Value::Bytes(Rc::default()),
        BytesOp::EmptyList => Value::List(Vec::new()),
    })
}

pub const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
pub const HEX_TRAP: &str = "hex";
pub const NIBBLE_BITS: u32 = 4;

pub fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

fn unhex(digits: &[u8], capacity: &ir::Constant) -> Result<Vec<u8>, String> {
    if digits.len() % 2 != 0 {
        return Err(HEX_TRAP.to_owned());
    }
    if (digits.len() / 2) as u64 > capacity.value {
        return Err(format!("bound({})", capacity.name));
    }
    digits
        .chunks_exact(2)
        .map(|pair| match (nibble(pair[0]), nibble(pair[1])) {
            (Some(high), Some(low)) => Ok((high << NIBBLE_BITS) | low),
            _ => Err(HEX_TRAP.to_owned()),
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX_DIGITS[usize::from(byte >> NIBBLE_BITS)]));
        out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

pub fn index(base: &Value, index: &Value) -> Result<Value, String> {
    let at = usize::try_from(index.as_u64()).map_err(|_| "index".to_owned())?;
    match base {
        Value::List(items) => items.get(at).cloned().ok_or_else(|| "index".to_owned()),
        Value::Bytes(buffer) => buffer
            .get(at)
            .map(|byte| Value::U64(u64::from(*byte)))
            .ok_or_else(|| "index".to_owned()),
        other => panic!("the checker types an indexed value, found {other:?}"),
    }
}

fn byte(value: &Value) -> Result<u8, String> {
    u8::try_from(value.as_u64()).map_err(|_| "byte".to_owned())
}

impl Frame<'_, '_> {
    fn eval(&self, expr: &Expr) -> Result<Value, String> {
        evaluate_with(self.program, &[], &self.locals, self.reads, expr)
    }

    fn capacity(&self, local: ir::LocalId) -> &ir::Bound {
        match &self.function.locals[local].ty {
            Type::List { capacity, .. } | Type::Bytes(capacity) => capacity,
            other => panic!("the checker types a local push, found {other:?}"),
        }
    }

    fn block(&mut self, statements: &[Stmt]) -> Result<Flow, String> {
        for statement in statements {
            if let Flow::Return(value) = self.statement(statement)? {
                return Ok(Flow::Return(value));
            }
        }
        Ok(Flow::Next)
    }

    fn statement(&mut self, statement: &Stmt) -> Result<Flow, String> {
        match statement {
            Stmt::Let { local, value } | Stmt::Set { local, value } => {
                let value = self.eval(value)?;
                self.locals[*local] = Some(value);
            }
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                let branch = if self.eval(condition)?.as_bool() {
                    then
                } else {
                    otherwise
                };
                return self.block(branch);
            }
            Stmt::Match { subject, arms, .. } => {
                let subject = self.eval(subject)?;
                let (variant, fields) = subject.as_variant();
                let arm = &arms[variant];
                for (binding, field) in arm.bindings.iter().zip(fields) {
                    if let Some(local) = binding {
                        self.locals[*local] = Some(field.clone());
                    }
                }
                return self.block(&arm.body);
            }
            Stmt::Loop { index, bound, body } => {
                for iteration in 0..bound.value {
                    self.locals[*index] = Some(Value::U64(iteration));
                    if let Flow::Return(value) = self.block(body)? {
                        return Ok(Flow::Return(value));
                    }
                }
            }
            Stmt::While {
                index,
                bound,
                condition,
                body,
            } => {
                for iteration in 0..bound.value {
                    self.locals[*index] = Some(Value::U64(iteration));
                    if !self.eval(condition)?.as_bool() {
                        return Ok(Flow::Next);
                    }
                    if let Flow::Return(value) = self.block(body)? {
                        return Ok(Flow::Return(value));
                    }
                }
            }
            Stmt::For {
                item, list, body, ..
            } => {
                let Value::List(items) = self.eval(list)? else {
                    panic!("the checker types a for loop's list");
                };
                for element in items {
                    self.locals[*item] = Some(element);
                    if let Flow::Return(value) = self.block(body)? {
                        return Ok(Flow::Return(value));
                    }
                }
            }
            Stmt::PushLocal {
                local,
                value,
                append,
            } => {
                let value = self.eval(value)?;
                let capacity = self.capacity(*local).clone();
                let slot = self.locals[*local]
                    .as_mut()
                    .expect("the checker binds a var before a push");
                let length = match (slot, value) {
                    (Value::List(items), value) => {
                        items.push(value);
                        items.len()
                    }
                    (Value::Bytes(buffer), Value::Bytes(more)) if *append => {
                        Rc::make_mut(buffer).extend_from_slice(&more);
                        buffer.len()
                    }
                    (Value::Bytes(buffer), value) => {
                        Rc::make_mut(buffer).push(byte(&value)?);
                        buffer.len()
                    }
                    (other, _) => panic!("the checker types a local push, found {other:?}"),
                };
                if length as u64 > capacity.value {
                    return Err(format!("bound({})", capacity.name));
                }
            }
            Stmt::SetIndex {
                local,
                index,
                value,
            } => {
                let at = self.eval(index)?.as_u64();
                let value = self.eval(value)?;
                let slot = self.locals[*local]
                    .as_mut()
                    .expect("the checker binds a var before a set");
                let at = usize::try_from(at).map_err(|_| "index".to_owned())?;
                match slot {
                    Value::List(items) => {
                        *items.get_mut(at).ok_or_else(|| "index".to_owned())? = value;
                    }
                    Value::Bytes(buffer) => {
                        let byte = byte(&value)?;
                        *Rc::make_mut(buffer)
                            .get_mut(at)
                            .ok_or_else(|| "index".to_owned())? = byte;
                    }
                    other => panic!("the checker types an indexed set, found {other:?}"),
                }
            }
            Stmt::Finish { value } => return Ok(Flow::Return(self.eval(value)?)),
            Stmt::Assign { .. }
            | Stmt::Push { .. }
            | Stmt::Shrink { .. }
            | Stmt::KeepBlobs { .. }
            | Stmt::Rewrite { .. }
            | Stmt::Parallel(_)
            | Stmt::Effect { .. }
            | Stmt::Take { .. } => panic!("the checker keeps effects and state out of a pure fn"),
        }
        Ok(Flow::Next)
    }
}
