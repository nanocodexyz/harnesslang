use super::ValType;
use super::decode::{BR_TABLE_LABELS_MAX, FUNCTION_LOCALS_MAX, Module, Reader};

const BLOCK_EMPTY: u8 = 0x40;
const MEMORY_ZERO: u8 = 0;
const MISC_MEMORY_COPY: u32 = 10;
const MISC_MEMORY_FILL: u32 = 11;
const I32_COMPARE_FIRST: u8 = 0x46;
const I32_COMPARE_LAST: u8 = 0x4f;
const I64_COMPARE_FIRST: u8 = 0x51;
const I64_COMPARE_LAST: u8 = 0x5a;
const I32_UNARY_FIRST: u8 = 0x67;
const I32_UNARY_LAST: u8 = 0x69;
const I32_ARITHMETIC_FIRST: u8 = 0x6a;
const I32_ARITHMETIC_LAST: u8 = 0x78;
const I64_UNARY_FIRST: u8 = 0x79;
const I64_UNARY_LAST: u8 = 0x7b;
const I64_ARITHMETIC_FIRST: u8 = 0x7c;
const I64_ARITHMETIC_LAST: u8 = 0x8a;
const COMPARISONS: usize = 10;
const UNARY_COUNTS: [Unary; 3] = [Unary::Clz, Unary::Ctz, Unary::Popcnt];
const BINARY: [Binary; 25] = [
    Binary::Eq,
    Binary::Ne,
    Binary::LtS,
    Binary::LtU,
    Binary::GtS,
    Binary::GtU,
    Binary::LeS,
    Binary::LeU,
    Binary::GeS,
    Binary::GeU,
    Binary::Add,
    Binary::Sub,
    Binary::Mul,
    Binary::DivS,
    Binary::DivU,
    Binary::RemS,
    Binary::RemU,
    Binary::And,
    Binary::Or,
    Binary::Xor,
    Binary::Shl,
    Binary::ShrS,
    Binary::ShrU,
    Binary::Rotl,
    Binary::Rotr,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Eqz,
    Clz,
    Ctz,
    Popcnt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    Eq,
    Ne,
    LtS,
    LtU,
    GtS,
    GtU,
    LeS,
    LeU,
    GeS,
    GeU,
    Add,
    Sub,
    Mul,
    DivS,
    DivU,
    RemS,
    RemU,
    And,
    Or,
    Xor,
    Shl,
    ShrS,
    ShrU,
    Rotl,
    Rotr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    I32,
    I64,
    I32S8,
    I32U8,
    I32S16,
    I32U16,
    I64S8,
    I64U8,
    I64S16,
    I64U16,
    I64S32,
    I64U32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    I32,
    I64,
    I32As8,
    I32As16,
    I64As8,
    I64As16,
    I64As32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Branch {
    pub target: u32,
    pub height: u32,
    pub arity: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Unreachable,
    Jump(u32),
    IfFalse(u32),
    Br(Branch),
    BrIf(Branch),
    BrTable { start: u32, labels: u32 },
    Return,
    Call(u32),
    CallImport(u32),
    Drop,
    Select,
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),
    Load(Load, u32),
    Store(Store, u32),
    MemorySize,
    MemoryGrow,
    MemoryCopy,
    MemoryFill,
    Const(u64),
    I32Unary(Unary),
    I64Unary(Unary),
    I32Binary(Binary),
    I64Binary(Binary),
    Wrap,
    ExtendS,
    ExtendU,
}

#[derive(Debug)]
pub struct Code {
    pub ops: Vec<Op>,
    pub table: Vec<Branch>,
    pub params: u32,
    pub locals: u32,
    pub result: u32,
    pub slots_max: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Function,
    Block,
    Loop,
    If,
    Else,
}

enum Patch {
    Op(usize),
    Table(usize),
}

struct Control {
    kind: Kind,
    result: Option<ValType>,
    height: usize,
    unreachable: bool,
    start: u32,
    else_at: Option<usize>,
    patches: Vec<Patch>,
}

struct Compiler<'m> {
    module: &'m Module,
    locals: Vec<ValType>,
    result: Option<ValType>,
    operands: Vec<Option<ValType>>,
    operands_max: usize,
    controls: Vec<Control>,
    ops: Vec<Op>,
    table: Vec<Branch>,
}

pub fn compile(module: &Module, index: usize, reader: &mut Reader) -> Result<Code, String> {
    let ty = &module.types[module.function_types[index] as usize];
    let mut locals = ty.params.clone();
    let groups = reader.count("local groups")?;
    for _ in 0..groups {
        let count = reader.u32()?;
        let ty = reader.value_type()?;
        let total = locals.len() as u64 + u64::from(count);
        if total > u64::from(FUNCTION_LOCALS_MAX) {
            return reader.fail(format!("{total} locals are over function_locals_max ({FUNCTION_LOCALS_MAX})"));
        }
        locals.extend(std::iter::repeat_n(ty, count as usize));
    }
    let mut compiler = Compiler {
        module,
        locals,
        result: ty.result,
        operands: Vec::new(),
        operands_max: 0,
        controls: Vec::new(),
        ops: Vec::new(),
        table: Vec::new(),
    };
    compiler.open(Kind::Function, ty.result, None);
    while !compiler.controls.is_empty() {
        let opcode = reader.byte()?;
        compiler.instruction(opcode, reader)?;
    }
    if !reader.done() {
        return reader.fail("a function body continues past its final end");
    }
    let params = ty.params.len() as u32;
    let all = compiler.locals.len() as u32;
    Ok(Code {
        ops: compiler.ops,
        table: compiler.table,
        params,
        locals: all - params,
        result: u32::from(ty.result.is_some()),
        slots_max: all + compiler.operands_max as u32,
    })
}

fn wrong<T>(reader: &Reader, message: impl std::fmt::Display) -> Result<T, String> {
    reader.fail(format!("invalid function body: {message}"))
}

impl Compiler<'_> {
    fn open(&mut self, kind: Kind, result: Option<ValType>, else_at: Option<usize>) {
        self.controls.push(Control {
            kind,
            result,
            height: self.operands.len(),
            unreachable: false,
            start: self.ops.len() as u32,
            else_at,
            patches: Vec::new(),
        });
    }

    fn push(&mut self, ty: Option<ValType>) {
        self.operands.push(ty);
        self.operands_max = self.operands_max.max(self.operands.len());
    }

    fn pop(&mut self, reader: &Reader) -> Result<Option<ValType>, String> {
        let control = self.controls.last().expect("a body is inside its function's control");
        if self.operands.len() == control.height {
            if control.unreachable {
                return Ok(None);
            }
            return wrong(reader, "the operand stack underflows");
        }
        Ok(self.operands.pop().expect("the operand stack is above the control's height"))
    }

    fn pop_expect(&mut self, reader: &Reader, expected: ValType) -> Result<(), String> {
        match self.pop(reader)? {
            Some(actual) if actual != expected => wrong(reader, format!("expected {} on the stack, found {}", expected.name(), actual.name())),
            _ => Ok(()),
        }
    }

    fn pop_result(&mut self, reader: &Reader, result: Option<ValType>) -> Result<(), String> {
        match result {
            Some(ty) => self.pop_expect(reader, ty),
            None => Ok(()),
        }
    }

    fn set_unreachable(&mut self) {
        let control = self.controls.last_mut().expect("a body is inside its function's control");
        self.operands.truncate(control.height);
        control.unreachable = true;
    }

    fn label(&self, reader: &Reader, depth: u32) -> Result<usize, String> {
        match (self.controls.len() - 1).checked_sub(depth as usize) {
            Some(index) => Ok(index),
            None => wrong(reader, format!("branch depth {depth} is past the outermost block")),
        }
    }

    fn label_type(&self, index: usize) -> Option<ValType> {
        let control = &self.controls[index];
        if control.kind == Kind::Loop { None } else { control.result }
    }

    fn branch(&mut self, index: usize, patch: Patch) -> Branch {
        let control = &mut self.controls[index];
        let height = (self.locals.len() + control.height) as u32;
        let arity = u32::from(control.kind != Kind::Loop && control.result.is_some());
        if control.kind == Kind::Loop {
            return Branch { target: control.start, height, arity };
        }
        control.patches.push(patch);
        Branch { target: u32::MAX, height, arity }
    }

    fn block_type(&self, reader: &mut Reader) -> Result<Option<ValType>, String> {
        let byte = reader.byte()?;
        match byte {
            BLOCK_EMPTY => Ok(None),
            0x7f => Ok(Some(ValType::I32)),
            0x7e => Ok(Some(ValType::I64)),
            _ => wrong(reader, format!("block type 0x{byte:02x} is not supported")),
        }
    }

    fn instruction(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        match opcode {
            0x00 => {
                self.ops.push(Op::Unreachable);
                self.set_unreachable();
            }
            0x01 => {}
            0x02 | 0x03 => {
                let result = self.block_type(reader)?;
                self.open(if opcode == 0x02 { Kind::Block } else { Kind::Loop }, result, None);
            }
            0x04 => {
                let result = self.block_type(reader)?;
                self.pop_expect(reader, ValType::I32)?;
                self.ops.push(Op::IfFalse(u32::MAX));
                self.open(Kind::If, result, Some(self.ops.len() - 1));
            }
            0x05 => self.otherwise(reader)?,
            0x0b => self.end(reader)?,
            0x0c..=0x0e => self.branch_instruction(opcode, reader)?,
            0x0f => {
                self.pop_result(reader, self.result)?;
                self.ops.push(Op::Return);
                self.set_unreachable();
            }
            0x10 => self.call(reader)?,
            0x1a => {
                self.pop(reader)?;
                self.ops.push(Op::Drop);
            }
            0x1b => self.select(reader)?,
            0x20..=0x24 => self.variable(opcode, reader)?,
            0x28..=0x3e => self.memory_access(opcode, reader)?,
            0x3f | 0x40 => {
                self.memory_index(reader)?;
                if opcode == 0x40 {
                    self.pop_expect(reader, ValType::I32)?;
                    self.ops.push(Op::MemoryGrow);
                } else {
                    self.ops.push(Op::MemorySize);
                }
                self.push(Some(ValType::I32));
            }
            0x41 => {
                let value = reader.s32()?;
                self.ops.push(Op::Const(u64::from(value as u32)));
                self.push(Some(ValType::I32));
            }
            0x42 => {
                let value = reader.s64()?;
                self.ops.push(Op::Const(value as u64));
                self.push(Some(ValType::I64));
            }
            0xfc => self.misc(reader)?,
            _ => self.numeric(opcode, reader)?,
        }
        Ok(())
    }

    fn otherwise(&mut self, reader: &mut Reader) -> Result<(), String> {
        let control = self.controls.last().expect("a body is inside its function's control");
        if control.kind != Kind::If {
            return wrong(reader, "else outside an if");
        }
        let result = control.result;
        self.pop_result(reader, result)?;
        let control = self.controls.last().expect("a body is inside its function's control");
        if self.operands.len() != control.height {
            return wrong(reader, "values are left on the stack at else");
        }
        self.ops.push(Op::Jump(u32::MAX));
        let jump = self.ops.len() - 1;
        let target = self.ops.len() as u32;
        let control = self.controls.last_mut().expect("a body is inside its function's control");
        let if_at = control.else_at.take().expect("an if records its branch");
        self.ops[if_at] = Op::IfFalse(target);
        control.patches.push(Patch::Op(jump));
        control.kind = Kind::Else;
        control.unreachable = false;
        Ok(())
    }

    fn end(&mut self, reader: &mut Reader) -> Result<(), String> {
        let result = self.controls.last().expect("a body is inside its function's control").result;
        self.pop_result(reader, result)?;
        let control = self.controls.pop().expect("a body is inside its function's control");
        if self.operands.len() != control.height {
            return wrong(reader, "values are left on the stack at end");
        }
        if control.kind == Kind::If && control.result.is_some() {
            return wrong(reader, "an if with a result needs an else");
        }
        let target = self.ops.len() as u32;
        if let Some(if_at) = control.else_at {
            self.ops[if_at] = Op::IfFalse(target);
        }
        for patch in control.patches {
            match patch {
                Patch::Op(at) => match &mut self.ops[at] {
                    Op::Jump(slot) => *slot = target,
                    Op::Br(branch) | Op::BrIf(branch) => branch.target = target,
                    other => unreachable!("a patch names a branch, not {other:?}"),
                },
                Patch::Table(at) => self.table[at].target = target,
            }
        }
        if control.kind == Kind::Function {
            self.ops.push(Op::Return);
        }
        if let Some(ty) = control.result {
            self.push(Some(ty));
        }
        Ok(())
    }

    fn branch_instruction(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        if opcode == 0x0e {
            return self.br_table(reader);
        }
        let depth = reader.u32()?;
        if opcode == 0x0d {
            self.pop_expect(reader, ValType::I32)?;
        }
        let index = self.label(reader, depth)?;
        let ty = self.label_type(index);
        self.pop_result(reader, ty)?;
        let at = self.ops.len();
        let branch = self.branch(index, Patch::Op(at));
        if opcode == 0x0c {
            self.ops.push(Op::Br(branch));
            self.set_unreachable();
        } else {
            self.ops.push(Op::BrIf(branch));
            if let Some(ty) = ty {
                self.push(Some(ty));
            }
        }
        Ok(())
    }

    fn br_table(&mut self, reader: &mut Reader) -> Result<(), String> {
        let labels = reader.u32()?;
        if labels > BR_TABLE_LABELS_MAX {
            return wrong(reader, format!("br_table has {labels} labels, over br_table_labels_max ({BR_TABLE_LABELS_MAX})"));
        }
        self.pop_expect(reader, ValType::I32)?;
        let start = self.table.len() as u32;
        let mut arity = None;
        for _ in 0..=labels {
            let depth = reader.u32()?;
            let index = self.label(reader, depth)?;
            let ty = self.label_type(index);
            if arity.is_some_and(|arity| arity != ty) {
                return wrong(reader, "br_table labels have different types");
            }
            arity = Some(ty);
            let at = self.table.len();
            let branch = self.branch(index, Patch::Table(at));
            self.table.push(branch);
        }
        let ty = arity.expect("a br_table has a default label");
        self.pop_result(reader, ty)?;
        self.ops.push(Op::BrTable { start, labels });
        self.set_unreachable();
        Ok(())
    }

    fn call(&mut self, reader: &mut Reader) -> Result<(), String> {
        let function = reader.u32()?;
        let imported = self.module.imports.len();
        if function as usize >= imported + self.module.function_types.len() {
            return wrong(reader, format!("function {function} does not exist"));
        }
        let ty = self.module.function_type(function);
        for param in ty.params.iter().rev() {
            self.pop_expect(reader, *param)?;
        }
        let result = ty.result;
        self.ops.push(match (function as usize).checked_sub(imported) {
            None => Op::CallImport(function),
            Some(local) => Op::Call(local as u32),
        });
        if let Some(result) = result {
            self.push(Some(result));
        }
        Ok(())
    }

    fn select(&mut self, reader: &mut Reader) -> Result<(), String> {
        self.pop_expect(reader, ValType::I32)?;
        let first = self.pop(reader)?;
        let second = self.pop(reader)?;
        let ty = match (first, second) {
            (Some(left), Some(right)) if left != right => return wrong(reader, "select operands differ in type"),
            (Some(ty), _) | (_, Some(ty)) => Some(ty),
            (None, None) => None,
        };
        self.ops.push(Op::Select);
        self.push(ty);
        Ok(())
    }

    fn variable(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        let index = reader.u32()?;
        if opcode <= 0x22 {
            let Some(&ty) = self.locals.get(index as usize) else {
                return wrong(reader, format!("local {index} does not exist"));
            };
            match opcode {
                0x20 => {
                    self.ops.push(Op::LocalGet(index));
                    self.push(Some(ty));
                }
                0x21 => {
                    self.pop_expect(reader, ty)?;
                    self.ops.push(Op::LocalSet(index));
                }
                _ => {
                    self.pop_expect(reader, ty)?;
                    self.ops.push(Op::LocalTee(index));
                    self.push(Some(ty));
                }
            }
            return Ok(());
        }
        let Some(global) = self.module.globals.get(index as usize).copied() else {
            return wrong(reader, format!("global {index} does not exist"));
        };
        if opcode == 0x23 {
            self.ops.push(Op::GlobalGet(index));
            self.push(Some(global.ty));
        } else {
            if !global.mutable {
                return wrong(reader, format!("global {index} is immutable"));
            }
            self.pop_expect(reader, global.ty)?;
            self.ops.push(Op::GlobalSet(index));
        }
        Ok(())
    }

    fn memory_index(&mut self, reader: &mut Reader) -> Result<(), String> {
        if self.module.memory.is_none() {
            return wrong(reader, "a memory instruction in a module without memory");
        }
        if reader.byte()? != MEMORY_ZERO {
            return wrong(reader, "only memory 0 exists");
        }
        Ok(())
    }

    fn memory_access(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        let (load, store, ty, natural) = access(opcode).map_or_else(
            || wrong(reader, format!("opcode 0x{opcode:02x} (floating point) is not supported")),
            Ok,
        )?;
        if self.module.memory.is_none() {
            return wrong(reader, "a memory instruction in a module without memory");
        }
        let align = reader.u32()?;
        let offset = reader.u32()?;
        if align > natural {
            return wrong(reader, format!("alignment 2^{align} is over the natural 2^{natural}"));
        }
        if let Some(load) = load {
            self.pop_expect(reader, ValType::I32)?;
            self.ops.push(Op::Load(load, offset));
            self.push(Some(ty));
        } else {
            self.pop_expect(reader, ty)?;
            self.pop_expect(reader, ValType::I32)?;
            self.ops.push(Op::Store(store.expect("an access loads or stores"), offset));
        }
        Ok(())
    }

    fn misc(&mut self, reader: &mut Reader) -> Result<(), String> {
        let code = reader.u32()?;
        match code {
            MISC_MEMORY_COPY => {
                self.memory_index(reader)?;
                if reader.byte()? != MEMORY_ZERO {
                    return wrong(reader, "only memory 0 exists");
                }
                self.ops.push(Op::MemoryCopy);
            }
            MISC_MEMORY_FILL => {
                self.memory_index(reader)?;
                self.ops.push(Op::MemoryFill);
            }
            _ => return wrong(reader, format!("opcode 0xfc {code} is not supported")),
        }
        for _ in 0..3 {
            self.pop_expect(reader, ValType::I32)?;
        }
        Ok(())
    }

    fn numeric(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        use ValType::{I32, I64};
        let (op, input, output) = match opcode {
            0x45 => (Op::I32Unary(Unary::Eqz), I32, I32),
            0x50 => (Op::I64Unary(Unary::Eqz), I64, I32),
            I32_COMPARE_FIRST..=I32_COMPARE_LAST => (Op::I32Binary(BINARY[usize::from(opcode - I32_COMPARE_FIRST)]), I32, I32),
            I64_COMPARE_FIRST..=I64_COMPARE_LAST => (Op::I64Binary(BINARY[usize::from(opcode - I64_COMPARE_FIRST)]), I64, I32),
            I32_UNARY_FIRST..=I32_UNARY_LAST => (Op::I32Unary(UNARY_COUNTS[usize::from(opcode - I32_UNARY_FIRST)]), I32, I32),
            I64_UNARY_FIRST..=I64_UNARY_LAST => (Op::I64Unary(UNARY_COUNTS[usize::from(opcode - I64_UNARY_FIRST)]), I64, I64),
            I32_ARITHMETIC_FIRST..=I32_ARITHMETIC_LAST => {
                (Op::I32Binary(BINARY[COMPARISONS + usize::from(opcode - I32_ARITHMETIC_FIRST)]), I32, I32)
            }
            I64_ARITHMETIC_FIRST..=I64_ARITHMETIC_LAST => {
                (Op::I64Binary(BINARY[COMPARISONS + usize::from(opcode - I64_ARITHMETIC_FIRST)]), I64, I64)
            }
            0xa7 => (Op::Wrap, I64, I32),
            0xac => (Op::ExtendS, I32, I64),
            0xad => (Op::ExtendU, I32, I64),
            _ => return wrong(reader, format!("opcode 0x{opcode:02x} is not supported")),
        };
        if matches!(op, Op::I32Binary(_) | Op::I64Binary(_)) {
            self.pop_expect(reader, input)?;
        }
        self.pop_expect(reader, input)?;
        self.ops.push(op);
        self.push(Some(output));
        Ok(())
    }
}

fn access(opcode: u8) -> Option<(Option<Load>, Option<Store>, ValType, u32)> {
    use ValType::{I32, I64};
    let load = |kind, ty, natural| Some((Some(kind), None, ty, natural));
    let store = |kind, ty, natural| Some((None, Some(kind), ty, natural));
    match opcode {
        0x28 => load(Load::I32, I32, 2),
        0x29 => load(Load::I64, I64, 3),
        0x2c => load(Load::I32S8, I32, 0),
        0x2d => load(Load::I32U8, I32, 0),
        0x2e => load(Load::I32S16, I32, 1),
        0x2f => load(Load::I32U16, I32, 1),
        0x30 => load(Load::I64S8, I64, 0),
        0x31 => load(Load::I64U8, I64, 0),
        0x32 => load(Load::I64S16, I64, 1),
        0x33 => load(Load::I64U16, I64, 1),
        0x34 => load(Load::I64S32, I64, 2),
        0x35 => load(Load::I64U32, I64, 2),
        0x36 => store(Store::I32, I32, 2),
        0x37 => store(Store::I64, I64, 3),
        0x3a => store(Store::I32As8, I32, 0),
        0x3b => store(Store::I32As16, I32, 1),
        0x3c => store(Store::I64As8, I64, 0),
        0x3d => store(Store::I64As16, I64, 1),
        0x3e => store(Store::I64As32, I64, 2),
        _ => None,
    }
}
