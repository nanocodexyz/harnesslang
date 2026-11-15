use super::ValType;
use super::decode::{BR_TABLE_LABELS_MAX, FUNCTION_LOCALS_MAX, Module, Reader};
use super::exec;
use super::fuse::{self, Condition};
use super::specialize::{Inst, Loops, fuse_loops, specialize};

const BLOCK_EMPTY: u8 = 0x40;
const MEMORY_ZERO: u8 = 0;
const MISC_MEMORY_COPY: u32 = 10;
const MISC_MEMORY_FILL: u32 = 11;
const MISC_MEMORY_OPERANDS: usize = 3;
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

impl Binary {
    pub const fn is_compare(self) -> bool {
        matches!(
            self,
            Self::Eq
                | Self::Ne
                | Self::LtS
                | Self::LtU
                | Self::GtS
                | Self::GtU
                | Self::LeS
                | Self::LeU
                | Self::GeS
                | Self::GeU
        )
    }

    pub fn negated(self) -> Self {
        match self {
            Self::Eq => Self::Ne,
            Self::Ne => Self::Eq,
            Self::LtS => Self::GeS,
            Self::LtU => Self::GeU,
            Self::GtS => Self::LeS,
            Self::GtU => Self::LeU,
            Self::LeS => Self::GtS,
            Self::LeU => Self::GtU,
            Self::GeS => Self::LtS,
            Self::GeU => Self::LtU,
            other => panic!("only a comparison negates, not {other:?}"),
        }
    }

    pub const fn swapped(self) -> Option<Self> {
        Some(match self {
            Self::Eq | Self::Ne | Self::Add | Self::Mul | Self::And | Self::Or | Self::Xor => self,
            Self::LtS => Self::GtS,
            Self::LtU => Self::GtU,
            Self::GtS => Self::LtS,
            Self::GtU => Self::LtU,
            Self::LeS => Self::GeS,
            Self::LeU => Self::GeU,
            Self::GeS => Self::LeS,
            Self::GeU => Self::LeU,
            _ => return None,
        })
    }
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
    pub source: u32,
    pub result: u32,
    pub arity: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Unreachable,
    Jump(u32),
    JumpIf {
        condition: u32,
        target: u32,
    },
    JumpUnless {
        condition: u32,
        target: u32,
    },
    JumpCompare {
        compare: Binary,
        left: u32,
        right: u32,
        target: u32,
    },
    JumpCompareImmediate {
        compare: Binary,
        left: u32,
        right: u32,
        target: u32,
    },
    JumpTable {
        index: u32,
        start: u32,
        labels: u32,
    },
    Return {
        source: u32,
    },
    ReturnVoid,
    Call {
        function: u32,
        base: u32,
    },
    CallImport {
        import: u32,
        base: u32,
    },
    Move {
        result: u32,
        source: u32,
    },
    Const {
        result: u32,
        value: u64,
    },
    Select {
        result: u32,
        first: u32,
        second: u32,
        condition: u32,
    },
    GlobalGet {
        result: u32,
        global: u32,
    },
    GlobalSet {
        global: u32,
        source: u32,
    },
    Load {
        load: Load,
        result: u32,
        address: u32,
        offset: u32,
    },
    Store {
        store: Store,
        address: u32,
        value: u32,
        offset: u32,
    },
    LoadPlus {
        load: Load,
        result: u32,
        address: u32,
        plus: u32,
    },
    LoadIndexed {
        load: Load,
        result: u32,
        base: u32,
        index: u32,
    },
    StoreIndexed {
        store: Store,
        base: u32,
        index: u32,
        value: u32,
    },
    I64BinaryConstant {
        binary: Binary,
        result: u32,
        left: u32,
        constant: u32,
    },
    MemorySize {
        result: u32,
    },
    MemoryGrow {
        result: u32,
        delta: u32,
    },
    MemoryCopy {
        destination: u32,
        source: u32,
        length: u32,
    },
    MemoryFill {
        destination: u32,
        value: u32,
        length: u32,
    },
    I32Unary {
        unary: Unary,
        result: u32,
        source: u32,
    },
    I64Unary {
        unary: Unary,
        result: u32,
        source: u32,
    },
    I32Binary {
        binary: Binary,
        result: u32,
        left: u32,
        right: u32,
    },
    I32BinaryImmediate {
        binary: Binary,
        result: u32,
        left: u32,
        right: u32,
    },
    I64Binary {
        binary: Binary,
        result: u32,
        left: u32,
        right: u32,
    },
    Wrap {
        result: u32,
        source: u32,
    },
    ExtendS {
        result: u32,
        source: u32,
    },
}

#[derive(Debug)]
pub struct Code {
    pub insts: Vec<Inst>,
    pub loops: Loops,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operand {
    Local(u32),
    Constant(u64),
    Stack,
}

#[derive(Clone, Copy)]
struct Entry {
    ty: Option<ValType>,
    operand: Operand,
    depth: usize,
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
    operands: Vec<(Option<ValType>, Operand)>,
    operands_max: usize,
    controls: Vec<Control>,
    ops: Vec<Op>,
    table: Vec<Branch>,
    constants: Vec<u64>,
    barrier: usize,
    inline_args: Vec<Vec<(ValType, Operand)>>,
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
            return reader.fail(format!(
                "{total} locals are over function_locals_max ({FUNCTION_LOCALS_MAX})"
            ));
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
        constants: Vec::new(),
        barrier: 0,
        inline_args: Vec::new(),
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
    let slots_max = all + compiler.operands_max as u32;
    fuse::assert_slots(&compiler.ops, &compiler.table, slots_max);
    fuse::assert_constants(&compiler.ops, compiler.constants.len());
    let mut insts = specialize(&compiler.ops, &compiler.constants);
    let loops = fuse_loops(&mut insts);
    Ok(Code {
        insts,
        loops,
        table: compiler.table,
        params,
        locals: all - params,
        result: u32::from(ty.result.is_some()),
        slots_max,
    })
}

fn wrong<T>(reader: &Reader, message: impl std::fmt::Display) -> Result<T, String> {
    reader.fail(format!("invalid function body: {message}"))
}

impl Compiler<'_> {
    fn slot(&self, depth: usize) -> u32 {
        (self.locals.len() + depth) as u32
    }

    fn reachable(&self) -> bool {
        self.controls
            .last()
            .is_none_or(|control| !control.unreachable)
    }

    fn emit(&mut self, op: Op) -> Option<usize> {
        if !self.reachable() {
            return None;
        }
        self.ops.push(op);
        Some(self.ops.len() - 1)
    }

    fn last_after_barrier(&self) -> Option<Op> {
        if !self.reachable() || self.ops.len() <= self.barrier {
            return None;
        }
        self.ops.last().copied()
    }

    fn open(&mut self, kind: Kind, result: Option<ValType>, else_at: Option<usize>) {
        if kind != Kind::Function {
            self.materialize_locals(None);
        }
        if kind == Kind::Loop {
            self.barrier = self.ops.len();
        }
        if result.is_some() {
            self.operands_max = self.operands_max.max(self.operands.len() + 1);
        }
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

    fn materialize_locals(&mut self, local: Option<u32>) {
        for depth in 0..self.operands.len() {
            if let (_, Operand::Local(index)) = self.operands[depth]
                && local.is_none_or(|local| local == index)
            {
                let result = self.slot(depth);
                self.emit(Op::Move {
                    result,
                    source: index,
                });
                self.operands[depth].1 = Operand::Stack;
            }
        }
    }

    fn push(&mut self, ty: Option<ValType>, operand: Operand) {
        self.operands.push((ty, operand));
        self.operands_max = self.operands_max.max(self.operands.len());
    }

    fn push_stack(&mut self, ty: Option<ValType>) -> u32 {
        let result = self.slot(self.operands.len());
        self.push(ty, Operand::Stack);
        result
    }

    fn pop(&mut self, reader: &Reader) -> Result<Entry, String> {
        let control = self
            .controls
            .last()
            .expect("a body is inside its function's control");
        if self.operands.len() == control.height {
            if control.unreachable {
                return Ok(Entry {
                    ty: None,
                    operand: Operand::Stack,
                    depth: self.operands.len(),
                });
            }
            return wrong(reader, "the operand stack underflows");
        }
        let (ty, operand) = self
            .operands
            .pop()
            .expect("the operand stack is above the control's height");
        Ok(Entry {
            ty,
            operand,
            depth: self.operands.len(),
        })
    }

    fn pop_expect(&mut self, reader: &Reader, expected: ValType) -> Result<Entry, String> {
        let entry = self.pop(reader)?;
        match entry.ty {
            Some(actual) if actual != expected => wrong(
                reader,
                format!(
                    "expected {} on the stack, found {}",
                    expected.name(),
                    actual.name()
                ),
            ),
            _ => Ok(entry),
        }
    }

    fn pop_result(
        &mut self,
        reader: &Reader,
        result: Option<ValType>,
    ) -> Result<Option<Entry>, String> {
        match result {
            Some(ty) => self.pop_expect(reader, ty).map(Some),
            None => Ok(None),
        }
    }

    fn read(&mut self, entry: Entry) -> u32 {
        match entry.operand {
            Operand::Local(index) => index,
            Operand::Stack => self.slot(entry.depth),
            Operand::Constant(value) => {
                let result = self.slot(entry.depth);
                self.emit(Op::Const { result, value });
                result
            }
        }
    }

    fn transfer(&mut self, entry: Entry, result: u32) {
        match entry.operand {
            Operand::Local(source) => {
                self.emit(Op::Move { result, source });
            }
            Operand::Constant(value) => {
                self.emit(Op::Const { result, value });
            }
            Operand::Stack => {
                let source = self.slot(entry.depth);
                if source != result {
                    self.emit(Op::Move { result, source });
                }
            }
        }
    }

    fn condition(&mut self, entry: Entry) -> Condition {
        if entry.operand == Operand::Stack {
            let slot = self.slot(entry.depth);
            if let Some(last) = self.last_after_barrier()
                && let Some(condition) = fuse::condition(last, slot)
            {
                self.ops.pop();
                return condition;
            }
        }
        Condition::NonZero(self.read(entry))
    }

    fn set_unreachable(&mut self) {
        let control = self
            .controls
            .last_mut()
            .expect("a body is inside its function's control");
        self.operands.truncate(control.height);
        control.unreachable = true;
    }

    fn label(&self, reader: &Reader, depth: u32) -> Result<usize, String> {
        match (self.controls.len() - 1).checked_sub(depth as usize) {
            Some(index) => Ok(index),
            None => wrong(
                reader,
                format!("branch depth {depth} is past the outermost block"),
            ),
        }
    }

    fn label_type(&self, index: usize) -> Option<ValType> {
        let control = &self.controls[index];
        if control.kind == Kind::Loop {
            None
        } else {
            control.result
        }
    }

    fn label_slot(&self, index: usize) -> u32 {
        self.slot(self.controls[index].height)
    }

    fn jump_to(&mut self, index: usize, condition: Option<Condition>) {
        let control = &self.controls[index];
        let target = if control.kind == Kind::Loop {
            control.start
        } else {
            u32::MAX
        };
        let op = condition.map_or(Op::Jump(target), |condition| {
            fuse::jump(condition, true, target)
        });
        if let Some(at) = self.emit(op)
            && self.controls[index].kind != Kind::Loop
        {
            self.controls[index].patches.push(Patch::Op(at));
        }
    }

    fn table_branch(&mut self, index: usize, source: u32) {
        let at = self.table.len();
        let control = &mut self.controls[index];
        let arity = u32::from(control.kind != Kind::Loop && control.result.is_some());
        let result = (self.locals.len() + control.height) as u32;
        let target = if control.kind == Kind::Loop {
            control.start
        } else {
            control.patches.push(Patch::Table(at));
            u32::MAX
        };
        self.table.push(Branch {
            target,
            source,
            result,
            arity,
        });
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
                self.emit(Op::Unreachable);
                self.set_unreachable();
            }
            0x01 => {}
            0x02 | 0x03 => {
                let result = self.block_type(reader)?;
                let kind = if opcode == 0x02 {
                    Kind::Block
                } else {
                    Kind::Loop
                };
                self.open(kind, result, None);
            }
            0x04 => {
                let result = self.block_type(reader)?;
                let entry = self.pop_expect(reader, ValType::I32)?;
                let condition = self.condition(entry);
                self.materialize_locals(None);
                let at = self.emit(fuse::jump(condition, false, u32::MAX));
                self.open(Kind::If, result, at);
            }
            0x05 => self.otherwise(reader)?,
            0x0b => self.end(reader)?,
            0x0c..=0x0e => self.branch_instruction(opcode, reader)?,
            0x0f => {
                let result = self.pop_result(reader, self.result)?;
                let op = match result {
                    Some(entry) => Op::Return {
                        source: self.read(entry),
                    },
                    None => Op::ReturnVoid,
                };
                self.emit(op);
                self.set_unreachable();
            }
            0x10 => self.call(reader)?,
            0x1a => {
                self.pop(reader)?;
            }
            0x1b => self.select(reader)?,
            0x20..=0x22 => self.local(opcode, reader)?,
            0x23 | 0x24 => self.global(opcode, reader)?,
            0x28..=0x3e => self.memory_access(opcode, reader)?,
            0x3f | 0x40 => self.memory_size(opcode, reader)?,
            0x41 => {
                let value = reader.s32()?;
                self.push(
                    Some(ValType::I32),
                    Operand::Constant(u64::from(value as u32)),
                );
            }
            0x42 => {
                let value = reader.s64()?;
                self.push(Some(ValType::I64), Operand::Constant(value as u64));
            }
            0xfc => self.misc(reader)?,
            _ => self.numeric(opcode, reader)?,
        }
        Ok(())
    }

    fn end_value(&mut self, reader: &Reader) -> Result<(), String> {
        let control = self
            .controls
            .last()
            .expect("a body is inside its function's control");
        let (result, height) = (control.result, control.height);
        if let Some(entry) = self.pop_result(reader, result)? {
            let slot = self.slot(height);
            self.transfer(entry, slot);
        }
        if self.operands.len() != height {
            return wrong(reader, "values are left on the stack at the end of a block");
        }
        Ok(())
    }

    fn otherwise(&mut self, reader: &mut Reader) -> Result<(), String> {
        let control = self
            .controls
            .last()
            .expect("a body is inside its function's control");
        if control.kind != Kind::If {
            return wrong(reader, "else outside an if");
        }
        self.end_value(reader)?;
        let jump = self.emit(Op::Jump(u32::MAX));
        let target = self.ops.len() as u32;
        self.barrier = self.ops.len();
        let control = self
            .controls
            .last_mut()
            .expect("a body is inside its function's control");
        if let Some(if_at) = control.else_at.take() {
            fuse::retarget(&mut self.ops[if_at], target);
        }
        if let Some(jump) = jump {
            control.patches.push(Patch::Op(jump));
        }
        control.kind = Kind::Else;
        control.unreachable = false;
        Ok(())
    }

    fn end(&mut self, reader: &mut Reader) -> Result<(), String> {
        self.end_value(reader)?;
        let control = self
            .controls
            .pop()
            .expect("a body is inside its function's control");
        if control.kind == Kind::If && control.result.is_some() {
            return wrong(reader, "an if with a result needs an else");
        }
        let target = self.ops.len() as u32;
        self.barrier = self.ops.len();
        if let Some(if_at) = control.else_at {
            fuse::retarget(&mut self.ops[if_at], target);
        }
        for patch in control.patches {
            match patch {
                Patch::Op(at) => fuse::retarget(&mut self.ops[at], target),
                Patch::Table(at) => self.table[at].target = target,
            }
        }
        if control.kind == Kind::Function {
            let op = if control.result.is_some() {
                Op::Return {
                    source: self.slot(control.height),
                }
            } else {
                Op::ReturnVoid
            };
            self.ops.push(op);
            return Ok(());
        }
        if let Some(ty) = control.result {
            self.push_stack(Some(ty));
        }
        Ok(())
    }

    fn branch_instruction(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        if opcode == 0x0e {
            return self.br_table(reader);
        }
        let depth = reader.u32()?;
        let condition = if opcode == 0x0d {
            Some(self.pop_expect(reader, ValType::I32)?)
        } else {
            None
        };
        let index = self.label(reader, depth)?;
        let ty = self.label_type(index);
        let value = self.pop_result(reader, ty)?;
        let label_slot = self.label_slot(index);
        let Some(condition) = condition else {
            if let Some(entry) = value {
                self.transfer(entry, label_slot);
            }
            self.jump_to(index, None);
            self.set_unreachable();
            return Ok(());
        };
        let condition = self.condition(condition);
        match value {
            None => self.jump_to(index, Some(condition)),
            Some(entry) => {
                let skip = self.emit(fuse::jump(condition, false, u32::MAX));
                self.transfer(entry, label_slot);
                self.jump_to(index, None);
                if let Some(skip) = skip {
                    let target = self.ops.len() as u32;
                    fuse::retarget(&mut self.ops[skip], target);
                    self.barrier = self.ops.len();
                }
                self.push(entry.ty, entry.operand);
            }
        }
        Ok(())
    }

    fn br_table(&mut self, reader: &mut Reader) -> Result<(), String> {
        let labels = reader.u32()?;
        if labels > BR_TABLE_LABELS_MAX {
            return wrong(
                reader,
                format!(
                    "br_table has {labels} labels, over br_table_labels_max ({BR_TABLE_LABELS_MAX})"
                ),
            );
        }
        let index_entry = self.pop_expect(reader, ValType::I32)?;
        let mut targets = Vec::with_capacity(labels as usize + 1);
        let mut arity = None;
        for _ in 0..=labels {
            let depth = reader.u32()?;
            let index = self.label(reader, depth)?;
            let ty = self.label_type(index);
            if arity.is_some_and(|arity| arity != ty) {
                return wrong(reader, "br_table labels have different types");
            }
            arity = Some(ty);
            targets.push(index);
        }
        let ty = arity.expect("a br_table has a default label");
        let value = self.pop_result(reader, ty)?;
        let index_slot = self.read(index_entry);
        let source = value.map_or(0, |entry| self.read(entry));
        let start = self.table.len() as u32;
        for index in targets {
            self.table_branch(index, source);
        }
        self.emit(Op::JumpTable {
            index: index_slot,
            start,
            labels,
        });
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
        let mut args = Vec::with_capacity(ty.params.len());
        for param in ty.params.iter().rev() {
            args.push(self.pop_expect(reader, *param)?);
        }
        let module = self.module;
        if let Some(local) = (function as usize).checked_sub(imported)
            && let Some(Some(body)) = module.inline.get(local)
        {
            return self.inline(local, &args, body, reader.offset());
        }
        for entry in args.iter().rev() {
            let slot = self.slot(entry.depth);
            self.transfer(*entry, slot);
        }
        let base = self.slot(self.operands.len());
        self.emit(match (function as usize).checked_sub(imported) {
            None => Op::CallImport {
                import: function,
                base,
            },
            Some(local) => Op::Call {
                function: local as u32,
                base,
            },
        });
        if let Some(result) = ty.result {
            self.push_stack(Some(result));
        }
        Ok(())
    }

    fn inline(
        &mut self,
        function: usize,
        args: &[Entry],
        body: &[u8],
        offset: usize,
    ) -> Result<(), String> {
        let module = self.module;
        let ty = &module.types[module.function_types[function] as usize];
        let depth = self.operands.len();
        let mut mapped = Vec::with_capacity(args.len());
        for (entry, param) in args.iter().rev().zip(&ty.params) {
            let operand = match entry.operand {
                Operand::Stack => Operand::Local(self.slot(entry.depth)),
                other => other,
            };
            mapped.push((*param, operand));
            self.push(entry.ty, Operand::Stack);
        }
        self.inline_args.push(mapped);
        let mut reader = Reader::new(body, offset);
        while !reader.done() {
            let opcode = reader.byte()?;
            self.instruction(opcode, &mut reader)?;
        }
        self.inline_args.pop();
        let result = self.pop_result(&reader, ty.result)?;
        if self.operands.len() != depth + args.len() {
            return wrong(&reader, "an inlined function leaves values on the stack");
        }
        self.operands.truncate(depth);
        let Some(entry) = result else {
            return Ok(());
        };
        match entry.operand {
            Operand::Local(index) if (index as usize) < self.locals.len() => {
                self.push(entry.ty, entry.operand);
            }
            Operand::Constant(_) => self.push(entry.ty, entry.operand),
            _ => {
                let slot = self.slot(depth);
                self.place(entry, slot);
                self.push_stack(entry.ty);
            }
        }
        Ok(())
    }

    fn place(&mut self, entry: Entry, slot: u32) {
        let retargeted = entry.operand == Operand::Stack
            && self
                .last_after_barrier()
                .and_then(fuse::result)
                .is_some_and(|result| result == self.slot(entry.depth));
        if retargeted {
            let last = self.ops.last_mut().expect("a retargeted op exists");
            fuse::set_result(last, slot);
        } else {
            self.transfer(entry, slot);
        }
    }

    fn select(&mut self, reader: &mut Reader) -> Result<(), String> {
        let condition = self.pop_expect(reader, ValType::I32)?;
        let second = self.pop(reader)?;
        let first = self.pop(reader)?;
        let ty = match (first.ty, second.ty) {
            (Some(left), Some(right)) if left != right => {
                return wrong(reader, "select operands differ in type");
            }
            (Some(ty), _) | (_, Some(ty)) => Some(ty),
            (None, None) => None,
        };
        let first_slot = self.read(first);
        let second_slot = self.read(second);
        let condition = self.read(condition);
        let result = self.push_stack(ty);
        self.emit(Op::Select {
            result,
            first: first_slot,
            second: second_slot,
            condition,
        });
        Ok(())
    }

    fn local(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        let index = reader.u32()?;
        if let Some(args) = self.inline_args.last() {
            assert_eq!(opcode, 0x20, "an inlined function only reads its params");
            let (ty, operand) = args[index as usize];
            self.push(Some(ty), operand);
            return Ok(());
        }
        let Some(&ty) = self.locals.get(index as usize) else {
            return wrong(reader, format!("local {index} does not exist"));
        };
        if opcode == 0x20 {
            self.push(Some(ty), Operand::Local(index));
            return Ok(());
        }
        let entry = self.pop_expect(reader, ty)?;
        self.materialize_locals(Some(index));
        self.place(entry, index);
        if opcode == 0x22 {
            self.push(Some(ty), Operand::Local(index));
        }
        Ok(())
    }

    fn global(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        let index = reader.u32()?;
        let Some(global) = self.module.globals.get(index as usize).copied() else {
            return wrong(reader, format!("global {index} does not exist"));
        };
        if opcode == 0x23 {
            if global.mutable {
                let result = self.push_stack(Some(global.ty));
                self.emit(Op::GlobalGet {
                    result,
                    global: index,
                });
            } else {
                self.push(Some(global.ty), Operand::Constant(global.init));
            }
            return Ok(());
        }
        if !global.mutable {
            return wrong(reader, format!("global {index} is immutable"));
        }
        let entry = self.pop_expect(reader, global.ty)?;
        let source = self.read(entry);
        self.emit(Op::GlobalSet {
            global: index,
            source,
        });
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

    fn memory_size(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        self.memory_index(reader)?;
        if opcode == 0x40 {
            let entry = self.pop_expect(reader, ValType::I32)?;
            let delta = self.read(entry);
            let result = self.push_stack(Some(ValType::I32));
            self.emit(Op::MemoryGrow { result, delta });
        } else {
            let result = self.push_stack(Some(ValType::I32));
            self.emit(Op::MemorySize { result });
        }
        Ok(())
    }

    fn memory_access(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        let (load, store, ty, natural) = access(opcode).map_or_else(
            || {
                wrong(
                    reader,
                    format!("opcode 0x{opcode:02x} (floating point) is not supported"),
                )
            },
            Ok,
        )?;
        if self.module.memory.is_none() {
            return wrong(reader, "a memory instruction in a module without memory");
        }
        let align = reader.u32()?;
        let offset = reader.u32()?;
        if align > natural {
            return wrong(
                reader,
                format!("alignment 2^{align} is over the natural 2^{natural}"),
            );
        }
        if let Some(load) = load {
            let entry = self.pop_expect(reader, ValType::I32)?;
            if offset == 0
                && let Some((base, index)) = self.indexed(entry)
            {
                let result = self.push_stack(Some(ty));
                self.emit(Op::LoadIndexed {
                    load,
                    result,
                    base,
                    index,
                });
                return Ok(());
            }
            if offset == 0
                && let Some((address, plus)) = self.plus(entry)
            {
                let result = self.push_stack(Some(ty));
                self.emit(Op::LoadPlus {
                    load,
                    result,
                    address,
                    plus,
                });
                return Ok(());
            }
            let address = self.read(entry);
            let result = self.push_stack(Some(ty));
            self.emit(Op::Load {
                load,
                result,
                address,
                offset,
            });
        } else {
            let value = self.pop_expect(reader, ty)?;
            let address = self.pop_expect(reader, ValType::I32)?;
            let store = store.expect("an access loads or stores");
            if offset == 0
                && let Operand::Local(value) = value.operand
                && let Some((base, index)) = self.indexed(address)
            {
                self.emit(Op::StoreIndexed {
                    store,
                    base,
                    index,
                    value,
                });
                return Ok(());
            }
            let address = self.read(address);
            let value = self.read(value);
            self.emit(Op::Store {
                store,
                address,
                value,
                offset,
            });
        }
        Ok(())
    }

    fn indexed(&mut self, entry: Entry) -> Option<(u32, u32)> {
        if entry.operand != Operand::Stack {
            return None;
        }
        let Some(Op::I32Binary {
            binary: Binary::Add,
            result,
            left,
            right,
        }) = self.last_after_barrier()
        else {
            return None;
        };
        if result != self.slot(entry.depth) {
            return None;
        }
        self.ops.pop();
        Some((left, right))
    }

    fn plus(&mut self, entry: Entry) -> Option<(u32, u32)> {
        if entry.operand != Operand::Stack {
            return None;
        }
        let Some(Op::I32BinaryImmediate {
            binary: Binary::Add,
            result,
            left,
            right,
        }) = self.last_after_barrier()
        else {
            return None;
        };
        if result != self.slot(entry.depth) {
            return None;
        }
        self.ops.pop();
        Some((left, right))
    }

    fn constant(&mut self, value: u64) -> u32 {
        let index = match self.constants.iter().position(|known| *known == value) {
            Some(index) => index,
            None => {
                self.constants.push(value);
                self.constants.len() - 1
            }
        };
        index as u32
    }

    fn misc(&mut self, reader: &mut Reader) -> Result<(), String> {
        let code = reader.u32()?;
        match code {
            MISC_MEMORY_COPY => {
                self.memory_index(reader)?;
                if reader.byte()? != MEMORY_ZERO {
                    return wrong(reader, "only memory 0 exists");
                }
            }
            MISC_MEMORY_FILL => self.memory_index(reader)?,
            _ => return wrong(reader, format!("opcode 0xfc {code} is not supported")),
        }
        let mut entries = [None; MISC_MEMORY_OPERANDS];
        for entry in entries.iter_mut().rev() {
            *entry = Some(self.pop_expect(reader, ValType::I32)?);
        }
        let [first, second, third] =
            entries.map(|entry| self.read(entry.expect("each memory operand is popped")));
        self.emit(if code == MISC_MEMORY_COPY {
            Op::MemoryCopy {
                destination: first,
                source: second,
                length: third,
            }
        } else {
            Op::MemoryFill {
                destination: first,
                value: second,
                length: third,
            }
        });
        Ok(())
    }

    fn binary(
        &mut self,
        binary: Binary,
        wide: bool,
        ty: ValType,
        reader: &Reader,
    ) -> Result<(), String> {
        let right = self.pop_expect(reader, ty)?;
        let left = self.pop_expect(reader, ty)?;
        let output = if wide && !binary.is_compare() {
            ValType::I64
        } else {
            ValType::I32
        };
        if let (Operand::Constant(left), Operand::Constant(right)) = (left.operand, right.operand) {
            let folded = if wide {
                exec::i64_binary(binary, left, right).ok()
            } else {
                exec::i32_binary(binary, left as u32, right as u32)
                    .ok()
                    .map(u64::from)
            };
            if let Some(value) = folded {
                self.push(Some(output), Operand::Constant(value));
                return Ok(());
            }
        }
        let op = match (left.operand, right.operand) {
            (_, Operand::Constant(value)) if !wide => {
                let left = self.read(left);
                Op::I32BinaryImmediate {
                    binary,
                    result: 0,
                    left,
                    right: value as u32,
                }
            }
            (_, Operand::Constant(value)) if wide => {
                let left = self.read(left);
                Op::I64BinaryConstant {
                    binary,
                    result: 0,
                    left,
                    constant: self.constant(value),
                }
            }
            (Operand::Constant(value), _) if wide && binary.swapped().is_some() => {
                let right = self.read(right);
                Op::I64BinaryConstant {
                    binary: binary.swapped().expect("a swappable binary swaps"),
                    result: 0,
                    left: right,
                    constant: self.constant(value),
                }
            }
            (Operand::Constant(value), _) if !wide && binary.swapped().is_some() => {
                let right = self.read(right);
                Op::I32BinaryImmediate {
                    binary: binary.swapped().expect("a swappable binary swaps"),
                    result: 0,
                    left: right,
                    right: value as u32,
                }
            }
            _ => {
                let (left, right) = (self.read(left), self.read(right));
                if wide {
                    Op::I64Binary {
                        binary,
                        result: 0,
                        left,
                        right,
                    }
                } else {
                    Op::I32Binary {
                        binary,
                        result: 0,
                        left,
                        right,
                    }
                }
            }
        };
        let result = self.push_stack(Some(output));
        let mut op = op;
        fuse::set_result(&mut op, result);
        self.emit(op);
        Ok(())
    }

    fn numeric(&mut self, opcode: u8, reader: &mut Reader) -> Result<(), String> {
        use ValType::{I32, I64};
        let (op, input, output) = match opcode {
            I32_COMPARE_FIRST..=I32_COMPARE_LAST => {
                return self.binary(
                    BINARY[usize::from(opcode - I32_COMPARE_FIRST)],
                    false,
                    I32,
                    reader,
                );
            }
            I64_COMPARE_FIRST..=I64_COMPARE_LAST => {
                return self.binary(
                    BINARY[usize::from(opcode - I64_COMPARE_FIRST)],
                    true,
                    I64,
                    reader,
                );
            }
            I32_ARITHMETIC_FIRST..=I32_ARITHMETIC_LAST => {
                let binary = BINARY[COMPARISONS + usize::from(opcode - I32_ARITHMETIC_FIRST)];
                return self.binary(binary, false, I32, reader);
            }
            I64_ARITHMETIC_FIRST..=I64_ARITHMETIC_LAST => {
                let binary = BINARY[COMPARISONS + usize::from(opcode - I64_ARITHMETIC_FIRST)];
                return self.binary(binary, true, I64, reader);
            }
            0x45 => (
                Op::I32Unary {
                    unary: Unary::Eqz,
                    result: 0,
                    source: 0,
                },
                I32,
                I32,
            ),
            0x50 => (
                Op::I64Unary {
                    unary: Unary::Eqz,
                    result: 0,
                    source: 0,
                },
                I64,
                I32,
            ),
            I32_UNARY_FIRST..=I32_UNARY_LAST => (
                Op::I32Unary {
                    unary: UNARY_COUNTS[usize::from(opcode - I32_UNARY_FIRST)],
                    result: 0,
                    source: 0,
                },
                I32,
                I32,
            ),
            I64_UNARY_FIRST..=I64_UNARY_LAST => (
                Op::I64Unary {
                    unary: UNARY_COUNTS[usize::from(opcode - I64_UNARY_FIRST)],
                    result: 0,
                    source: 0,
                },
                I64,
                I64,
            ),
            0xa7 => (
                Op::Wrap {
                    result: 0,
                    source: 0,
                },
                I64,
                I32,
            ),
            0xac => (
                Op::ExtendS {
                    result: 0,
                    source: 0,
                },
                I32,
                I64,
            ),
            0xad => (
                Op::Move {
                    result: 0,
                    source: 0,
                },
                I32,
                I64,
            ),
            _ => return wrong(reader, format!("opcode 0x{opcode:02x} is not supported")),
        };
        let entry = self.pop_expect(reader, input)?;
        let source = self.read(entry);
        let result = self.push_stack(Some(output));
        let mut op = op;
        fuse::set_unary(&mut op, result, source);
        self.emit(op);
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
