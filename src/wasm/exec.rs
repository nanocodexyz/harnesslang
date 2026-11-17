use super::compile::{Binary, Code, Load, Store, Unary};
use super::decode::{MEMORY_PAGES_MAX, Module};
use super::specialize::{Crc, Hex, Inst, Scan, Test, Unhex, Words};
use super::{Error, Host, Trap, Value};
use crate::emit::wasm::encode::PAGE_BYTES;

const BYTE_VALUES: u32 = 256;
const CRC_WORD_BYTES: usize = 4;
const CRC_TABLE_BYTES: u32 = BYTE_VALUES * CRC_WORD_BYTES as u32;
const HEX_DIGIT_COUNT: u32 = 16;
pub const NODE_STACK_BYTES: usize = 984 * 1024;
pub const SLOT_BYTES: usize = 8;
pub const FRAME_BYTES_MIN: usize = 4 * SLOT_BYTES;
pub const VALUE_SLOTS_MAX: usize = NODE_STACK_BYTES / SLOT_BYTES;
pub const CALL_DEPTH_MAX: usize = NODE_STACK_BYTES / FRAME_BYTES_MIN;
const I32_BITS: u32 = 32;
const I64_BITS: u32 = 64;
const GROW_FAILED: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub call_depth_max: usize,
    pub value_slots_max: usize,
}

impl Limits {
    pub const NODE: Limits = Limits {
        call_depth_max: CALL_DEPTH_MAX,
        value_slots_max: VALUE_SLOTS_MAX,
    };
}

pub struct Memory {
    bytes: Vec<u8>,
    pages_max: u32,
}

impl Memory {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn pages(&self) -> u32 {
        (self.bytes.len() / PAGE_BYTES as usize) as u32
    }

    fn range(&self, at: u64, length: u64) -> Result<std::ops::Range<usize>, Trap> {
        let end = at + length;
        if end > self.bytes.len() as u64 {
            return Err(Trap::MemoryOutOfBounds);
        }
        Ok(at as usize..end as usize)
    }

    pub fn read(&self, at: u32, length: u32) -> Result<&[u8], Trap> {
        let range = self.range(u64::from(at), u64::from(length))?;
        Ok(&self.bytes[range])
    }

    pub fn write(&mut self, at: u32, bytes: &[u8]) -> Result<(), Trap> {
        let range = self.range(u64::from(at), bytes.len() as u64)?;
        self.bytes[range].copy_from_slice(bytes);
        Ok(())
    }

    fn grow(&mut self, delta: u32) -> u32 {
        let pages = self.pages();
        let wanted = u64::from(pages) + u64::from(delta);
        if wanted > u64::from(self.pages_max) {
            return GROW_FAILED;
        }
        self.bytes.resize(wanted as usize * PAGE_BYTES as usize, 0);
        pages
    }

    fn load<const N: usize>(&self, address: u64) -> Result<[u8; N], Trap> {
        let at = address as usize;
        match self.bytes.get(at..at + N) {
            Some(bytes) => Ok(bytes.try_into().expect("a range of n bytes is n bytes")),
            None => Err(Trap::MemoryOutOfBounds),
        }
    }

    fn span(&self, base: u32, from: u32, to: u32) -> Option<std::ops::Range<usize>> {
        let start = u64::from(base) + u64::from(from);
        let end = u64::from(base) + u64::from(to.max(from));
        (end <= u64::from(u32::MAX) && end <= self.bytes.len() as u64)
            .then_some(start as usize..end as usize)
    }

    fn byte(&self, address: u64) -> Result<u8, Trap> {
        self.bytes
            .get(address as usize)
            .copied()
            .ok_or(Trap::MemoryOutOfBounds)
    }

    fn store(&mut self, address: u64, bytes: &[u8]) -> Result<(), Trap> {
        let at = address as usize;
        match self.bytes.get_mut(at..at + bytes.len()) {
            Some(to) => {
                to.copy_from_slice(bytes);
                Ok(())
            }
            None => Err(Trap::MemoryOutOfBounds),
        }
    }
}

#[derive(Clone, Copy)]
struct Frame<'a> {
    code: &'a Code,
    pc: usize,
    base: usize,
}

pub struct Instance<'a> {
    module: &'a Module,
    memory: Memory,
    globals: Vec<u64>,
    stack: Vec<u64>,
    top: usize,
    depth: usize,
    limits: Limits,
    executed: u64,
}

impl<'a> Instance<'a> {
    pub fn new(module: &'a Module, limits: Limits) -> Instance<'a> {
        let (pages, pages_max) = module.memory.map_or((0, 0), |memory| {
            (memory.min, memory.max.unwrap_or(MEMORY_PAGES_MAX))
        });
        let mut memory = Memory {
            bytes: vec![0; pages as usize * PAGE_BYTES as usize],
            pages_max,
        };
        for segment in &module.data {
            memory
                .write(segment.offset, &segment.bytes)
                .expect("decoding checks every data segment fits the initial memory");
        }
        Instance {
            module,
            memory,
            globals: module.globals.iter().map(|global| global.init).collect(),
            stack: Vec::new(),
            top: 0,
            depth: 0,
            limits,
            executed: 0,
        }
    }

    pub fn executed(&self) -> u64 {
        self.executed
    }

    pub fn module(&self) -> &'a Module {
        self.module
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    pub fn memory_mut(&mut self) -> &mut Memory {
        &mut self.memory
    }

    pub fn invoke<H: Host>(
        &mut self,
        host: &mut H,
        function: u32,
        args: &[Value],
    ) -> Result<Option<Value>, Error<H::Stop>> {
        let module = self.module;
        let ty = module.function_type(function);
        assert_eq!(
            args.len(),
            ty.params.len(),
            "an invocation passes its function's params"
        );
        for (arg, param) in args.iter().zip(&ty.params) {
            assert_eq!(
                arg.ty(),
                *param,
                "an invocation passes values of its params' types"
            );
        }
        let Some(local) = (function as usize).checked_sub(module.imports.len()) else {
            let result = host.call(function as usize, args, self)?;
            assert_eq!(
                result.map(Value::ty),
                ty.result,
                "a host returns its import's result type"
            );
            return Ok(result);
        };
        let saved_top = self.top;
        let saved_depth = self.depth;
        let base = self.top;
        let mut stack = std::mem::take(&mut self.stack);
        if stack.len() < base + args.len() {
            stack.resize(base + args.len(), 0);
        }
        for (index, arg) in args.iter().enumerate() {
            stack[base + index] = arg.slot();
        }
        let ran = self.execute(host, &mut stack, local, base);
        let result = ty.result.map(|ty| Value::from_slot(ty, stack[base]));
        self.stack = stack;
        self.top = saved_top;
        self.depth = saved_depth;
        ran.map(|()| result)
    }

    fn enter(&mut self, stack: &mut Vec<u64>, code: &Code, base: usize) -> Result<(), Trap> {
        if self.depth >= self.limits.call_depth_max {
            return Err(Trap::CallStackExhausted);
        }
        let end = base + code.slots_max as usize;
        if end > self.limits.value_slots_max {
            return Err(Trap::ValueStackExhausted);
        }
        self.depth += 1;
        if stack.len() < end {
            stack.resize(end, 0);
        }
        let locals = base + code.params as usize;
        stack[locals..locals + code.locals as usize].fill(0);
        Ok(())
    }

    fn import<H: Host>(
        &mut self,
        host: &mut H,
        stack: &mut Vec<u64>,
        import: u32,
        first: usize,
    ) -> Result<(), Error<H::Stop>> {
        let ty = self.module.function_type(import);
        let args: Vec<Value> = ty
            .params
            .iter()
            .zip(&stack[first..first + ty.params.len()])
            .map(|(param, slot)| Value::from_slot(*param, *slot))
            .collect();
        std::mem::swap(&mut self.stack, stack);
        let saved_top = self.top;
        self.top = first;
        let called = host.call(import as usize, &args, self);
        self.top = saved_top;
        std::mem::swap(&mut self.stack, stack);
        let result = called?;
        assert_eq!(
            result.map(Value::ty),
            ty.result,
            "a host returns its import's result type"
        );
        if let Some(value) = result {
            stack[first] = value.slot();
        }
        Ok(())
    }

    fn execute<H: Host>(
        &mut self,
        host: &mut H,
        stack: &mut Vec<u64>,
        local: usize,
        base: usize,
    ) -> Result<(), Error<H::Stop>> {
        let mut executed = 0;
        let ran = self.run(host, stack, local, base, &mut executed);
        self.executed += executed;
        ran
    }

    fn run<H: Host>(
        &mut self,
        host: &mut H,
        stack: &mut Vec<u64>,
        local: usize,
        base: usize,
        counted: &mut u64,
    ) -> Result<(), Error<H::Stop>> {
        let module = self.module;
        let mut frames: Vec<Frame> = Vec::new();
        let mut code = &module.code[local];
        let mut base = base;
        self.enter(stack, code, base).map_err(Error::Trap)?;
        let mut pc = 0;
        let mut executed: u64 = 0;
        macro_rules! slot {
            ($index:expr) => {
                stack[base + $index as usize]
            };
        }
        macro_rules! check {
            ($value:expr) => {
                match $value {
                    Ok(value) => value,
                    Err(trap) => {
                        *counted += executed;
                        return Err(Error::Trap(trap));
                    }
                }
            };
        }
        macro_rules! at {
            ($address:expr, $offset:expr) => {
                u64::from(slot!($address) as u32) + u64::from($offset)
            };
        }
        macro_rules! plus {
            ($address:expr, $plus:expr) => {
                u64::from((slot!($address) as u32).wrapping_add($plus))
            };
        }
        macro_rules! indexed {
            ($left:expr, $index:expr) => {
                u64::from((slot!($left) as u32).wrapping_add(slot!($index) as u32))
            };
        }
        loop {
            let inst = code.insts[pc];
            pc += 1;
            executed += 1;
            match inst {
                Inst::Move { result, source } => slot!(result) = slot!(source),
                Inst::Const { result, value } => slot!(result) = value,
                Inst::JumpEq {
                    left,
                    right,
                    target,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    if left == right {
                        pc = target as usize;
                    }
                }
                Inst::JumpEqImmediate {
                    left,
                    right,
                    target,
                } => {
                    let left = slot!(left) as u32;
                    if left == right {
                        pc = target as usize;
                    }
                }
                Inst::JumpNe {
                    left,
                    right,
                    target,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    if left != right {
                        pc = target as usize;
                    }
                }
                Inst::JumpNeImmediate {
                    left,
                    right,
                    target,
                } => {
                    let left = slot!(left) as u32;
                    if left != right {
                        pc = target as usize;
                    }
                }
                Inst::JumpLtS {
                    left,
                    right,
                    target,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    if (left as i32) < (right as i32) {
                        pc = target as usize;
                    }
                }
                Inst::JumpLtSImmediate {
                    left,
                    right,
                    target,
                } => {
                    let left = slot!(left) as u32;
                    if (left as i32) < (right as i32) {
                        pc = target as usize;
                    }
                }
                Inst::JumpLtU {
                    left,
                    right,
                    target,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    if left < right {
                        pc = target as usize;
                    }
                }
                Inst::JumpLtUImmediate {
                    left,
                    right,
                    target,
                } => {
                    let left = slot!(left) as u32;
                    if left < right {
                        pc = target as usize;
                    }
                }
                Inst::JumpGtS {
                    left,
                    right,
                    target,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    if (left as i32) > (right as i32) {
                        pc = target as usize;
                    }
                }
                Inst::JumpGtSImmediate {
                    left,
                    right,
                    target,
                } => {
                    let left = slot!(left) as u32;
                    if (left as i32) > (right as i32) {
                        pc = target as usize;
                    }
                }
                Inst::JumpGtU {
                    left,
                    right,
                    target,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    if left > right {
                        pc = target as usize;
                    }
                }
                Inst::JumpGtUImmediate {
                    left,
                    right,
                    target,
                } => {
                    let left = slot!(left) as u32;
                    if left > right {
                        pc = target as usize;
                    }
                }
                Inst::JumpLeS {
                    left,
                    right,
                    target,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    if (left as i32) <= (right as i32) {
                        pc = target as usize;
                    }
                }
                Inst::JumpLeSImmediate {
                    left,
                    right,
                    target,
                } => {
                    let left = slot!(left) as u32;
                    if (left as i32) <= (right as i32) {
                        pc = target as usize;
                    }
                }
                Inst::JumpLeU {
                    left,
                    right,
                    target,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    if left <= right {
                        pc = target as usize;
                    }
                }
                Inst::JumpLeUImmediate {
                    left,
                    right,
                    target,
                } => {
                    let left = slot!(left) as u32;
                    if left <= right {
                        pc = target as usize;
                    }
                }
                Inst::JumpGeS {
                    left,
                    right,
                    target,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    if (left as i32) >= (right as i32) {
                        pc = target as usize;
                    }
                }
                Inst::JumpGeSImmediate {
                    left,
                    right,
                    target,
                } => {
                    let left = slot!(left) as u32;
                    if (left as i32) >= (right as i32) {
                        pc = target as usize;
                    }
                }
                Inst::JumpGeU {
                    left,
                    right,
                    target,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    if left >= right {
                        pc = target as usize;
                    }
                }
                Inst::JumpGeUImmediate {
                    left,
                    right,
                    target,
                } => {
                    let left = slot!(left) as u32;
                    if left >= right {
                        pc = target as usize;
                    }
                }
                Inst::I32Add {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(left.wrapping_add(right));
                }
                Inst::I32AddImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(left.wrapping_add(right));
                }
                Inst::I32Sub {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(left.wrapping_sub(right));
                }
                Inst::I32SubImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(left.wrapping_sub(right));
                }
                Inst::I32Mul {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(left.wrapping_mul(right));
                }
                Inst::I32MulImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(left.wrapping_mul(right));
                }
                Inst::I32And {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(left & right);
                }
                Inst::I32AndImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(left & right);
                }
                Inst::I32Or {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(left | right);
                }
                Inst::I32OrImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(left | right);
                }
                Inst::I32Xor {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(left ^ right);
                }
                Inst::I32XorImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(left ^ right);
                }
                Inst::I32Shl {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(left.wrapping_shl(right));
                }
                Inst::I32ShlImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(left.wrapping_shl(right));
                }
                Inst::I32ShrS {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from((left as i32).wrapping_shr(right) as u32);
                }
                Inst::I32ShrSImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from((left as i32).wrapping_shr(right) as u32);
                }
                Inst::I32ShrU {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(left.wrapping_shr(right));
                }
                Inst::I32ShrUImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(left.wrapping_shr(right));
                }
                Inst::I32Eq {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(u32::from(left == right));
                }
                Inst::I32EqImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(u32::from(left == right));
                }
                Inst::I32Ne {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(u32::from(left != right));
                }
                Inst::I32NeImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(u32::from(left != right));
                }
                Inst::I32LtS {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(u32::from((left as i32) < (right as i32)));
                }
                Inst::I32LtSImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(u32::from((left as i32) < (right as i32)));
                }
                Inst::I32LtU {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(u32::from(left < right));
                }
                Inst::I32LtUImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(u32::from(left < right));
                }
                Inst::I32GtS {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(u32::from((left as i32) > (right as i32)));
                }
                Inst::I32GtSImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(u32::from((left as i32) > (right as i32)));
                }
                Inst::I32GtU {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(u32::from(left > right));
                }
                Inst::I32GtUImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(u32::from(left > right));
                }
                Inst::I32LeS {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(u32::from((left as i32) <= (right as i32)));
                }
                Inst::I32LeSImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(u32::from((left as i32) <= (right as i32)));
                }
                Inst::I32LeU {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(u32::from(left <= right));
                }
                Inst::I32LeUImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(u32::from(left <= right));
                }
                Inst::I32GeS {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(u32::from((left as i32) >= (right as i32)));
                }
                Inst::I32GeSImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(u32::from((left as i32) >= (right as i32)));
                }
                Inst::I32GeU {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left) as u32, slot!(right) as u32);
                    slot!(result) = u64::from(u32::from(left >= right));
                }
                Inst::I32GeUImmediate {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left) as u32;
                    slot!(result) = u64::from(u32::from(left >= right));
                }
                Inst::I64Add {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left), slot!(right));
                    slot!(result) = left.wrapping_add(right);
                }
                Inst::I64AddConstant {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left);
                    slot!(result) = left.wrapping_add(right);
                }
                Inst::I64Sub {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left), slot!(right));
                    slot!(result) = left.wrapping_sub(right);
                }
                Inst::I64SubConstant {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left);
                    slot!(result) = left.wrapping_sub(right);
                }
                Inst::I64Mul {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left), slot!(right));
                    slot!(result) = left.wrapping_mul(right);
                }
                Inst::I64MulConstant {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left);
                    slot!(result) = left.wrapping_mul(right);
                }
                Inst::I64And {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left), slot!(right));
                    slot!(result) = left & right;
                }
                Inst::I64AndConstant {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left);
                    slot!(result) = left & right;
                }
                Inst::I64Or {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left), slot!(right));
                    slot!(result) = left | right;
                }
                Inst::I64OrConstant {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left);
                    slot!(result) = left | right;
                }
                Inst::I64Xor {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left), slot!(right));
                    slot!(result) = left ^ right;
                }
                Inst::I64XorConstant {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left);
                    slot!(result) = left ^ right;
                }
                Inst::I64Shl {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left), slot!(right));
                    slot!(result) = left.wrapping_shl(right as u32);
                }
                Inst::I64ShlConstant {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left);
                    slot!(result) = left.wrapping_shl(right as u32);
                }
                Inst::I64ShrU {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left), slot!(right));
                    slot!(result) = left.wrapping_shr(right as u32);
                }
                Inst::I64ShrUConstant {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left);
                    slot!(result) = left.wrapping_shr(right as u32);
                }
                Inst::I64Eq {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left), slot!(right));
                    slot!(result) = u64::from(left == right);
                }
                Inst::I64EqConstant {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left);
                    slot!(result) = u64::from(left == right);
                }
                Inst::I64Ne {
                    result,
                    left,
                    right,
                } => {
                    let (left, right) = (slot!(left), slot!(right));
                    slot!(result) = u64::from(left != right);
                }
                Inst::I64NeConstant {
                    result,
                    left,
                    right,
                } => {
                    let left = slot!(left);
                    slot!(result) = u64::from(left != right);
                }
                Inst::Scan { scan } => {
                    let scan = code.loops.scans[scan as usize];
                    let (target, ops) = check!(self.scan(scan, stack, base));
                    executed += ops - 1;
                    pc = target as usize;
                }
                Inst::Crc { crc } => {
                    let crc = code.loops.crcs[crc as usize];
                    let (target, ops) = check!(self.crc(crc, stack, base));
                    executed += ops - 1;
                    pc = target as usize;
                }
                Inst::Unhex { unhex } => {
                    let unhex = code.loops.unhexes[unhex as usize];
                    let (target, ops) = check!(self.unhex(unhex, stack, base));
                    executed += ops - 1;
                    pc = target as usize;
                }
                Inst::Hex { hex } => {
                    let hex = code.loops.hexes[hex as usize];
                    let (target, ops) = check!(self.hex(hex, stack, base));
                    executed += ops - 1;
                    pc = target as usize;
                }
                Inst::Words { words } => {
                    let words = code.loops.words[words as usize];
                    let (target, ops) = check!(self.words(words, stack, base));
                    executed += ops - 1;
                    pc = target as usize;
                }
                Inst::JumpIf { condition, target } => {
                    if slot!(condition) as u32 != 0 {
                        pc = target as usize;
                    }
                }
                Inst::JumpUnless { condition, target } => {
                    if slot!(condition) as u32 == 0 {
                        pc = target as usize;
                    }
                }
                Inst::Jump(target) => pc = target as usize,
                Inst::I32Eqz { result, source } => {
                    slot!(result) = u64::from(slot!(source) as u32 == 0);
                }
                Inst::Wrap { result, source } => slot!(result) = u64::from(slot!(source) as u32),
                Inst::ExtendS { result, source } => {
                    slot!(result) = i64::from(slot!(source) as u32 as i32) as u64;
                }
                Inst::LoadI32 {
                    result,
                    address,
                    offset,
                } => {
                    let bytes = check!(self.memory.load(at!(address, offset)));
                    slot!(result) = u64::from(u32::from_le_bytes(bytes));
                }
                Inst::LoadI64 {
                    result,
                    address,
                    offset,
                } => {
                    let bytes = check!(self.memory.load(at!(address, offset)));
                    slot!(result) = u64::from_le_bytes(bytes);
                }
                Inst::LoadU8 {
                    result,
                    address,
                    offset,
                } => slot!(result) = u64::from(check!(self.memory.byte(at!(address, offset)))),
                Inst::Load {
                    load,
                    result,
                    address,
                    offset,
                } => slot!(result) = check!(self.load(load, at!(address, offset))),
                Inst::LoadPlusI32 {
                    result,
                    address,
                    plus,
                } => {
                    let bytes = check!(self.memory.load(plus!(address, plus)));
                    slot!(result) = u64::from(u32::from_le_bytes(bytes));
                }
                Inst::LoadPlusU8 {
                    result,
                    address,
                    plus,
                } => slot!(result) = u64::from(check!(self.memory.byte(plus!(address, plus)))),
                Inst::LoadPlus {
                    load,
                    result,
                    address,
                    plus,
                } => slot!(result) = check!(self.load(load, plus!(address, plus))),
                Inst::LoadIndexedI32 {
                    result,
                    base: left,
                    index,
                } => {
                    let bytes = check!(self.memory.load(indexed!(left, index)));
                    slot!(result) = u64::from(u32::from_le_bytes(bytes));
                }
                Inst::LoadIndexedU8 {
                    result,
                    base: left,
                    index,
                } => slot!(result) = u64::from(check!(self.memory.byte(indexed!(left, index)))),
                Inst::LoadIndexed {
                    load,
                    result,
                    base: left,
                    index,
                } => slot!(result) = check!(self.load(load, indexed!(left, index))),
                Inst::StoreI32 {
                    address,
                    value,
                    offset,
                } => {
                    let bytes = (slot!(value) as u32).to_le_bytes();
                    check!(self.memory.store(at!(address, offset), &bytes));
                }
                Inst::StoreI64 {
                    address,
                    value,
                    offset,
                } => {
                    let bytes = slot!(value).to_le_bytes();
                    check!(self.memory.store(at!(address, offset), &bytes));
                }
                Inst::StoreU8 {
                    address,
                    value,
                    offset,
                } => check!(
                    self.memory
                        .store(at!(address, offset), &[slot!(value) as u8])
                ),
                Inst::Store {
                    store,
                    address,
                    value,
                    offset,
                } => check!(self.store(store, at!(address, offset), slot!(value))),
                Inst::StoreIndexed {
                    store,
                    base: left,
                    index,
                    value,
                } => check!(self.store(store, indexed!(left, index), slot!(value))),
                Inst::GlobalGet { result, global } => slot!(result) = self.globals[global as usize],
                Inst::GlobalSet { global, source } => self.globals[global as usize] = slot!(source),
                Inst::Call { function, base: at } => {
                    let callee = &module.code[function as usize];
                    let callee_base = base + at as usize;
                    frames.push(Frame { code, pc, base });
                    check!(self.enter(stack, callee, callee_base));
                    code = callee;
                    base = callee_base;
                    pc = 0;
                }
                Inst::Return { .. } | Inst::ReturnVoid => {
                    if let Inst::Return { source } = inst {
                        stack[base] = slot!(source);
                    }
                    self.depth -= 1;
                    let Some(frame) = frames.pop() else {
                        *counted += executed;
                        return Ok(());
                    };
                    code = frame.code;
                    pc = frame.pc;
                    base = frame.base;
                }
                Inst::CallImport { import, base: at } => {
                    *counted += executed;
                    executed = 0;
                    self.import(host, stack, import, base + at as usize)?;
                }
                Inst::JumpTable {
                    index,
                    start,
                    labels,
                } => {
                    let index = (slot!(index) as u32).min(labels);
                    let branch = code.table[(start + index) as usize];
                    if branch.arity == 1 {
                        slot!(branch.result) = slot!(branch.source);
                    }
                    pc = branch.target as usize;
                }
                _ => check!(self.other(inst, stack, base)),
            }
        }
    }

    fn scan(&self, scan: Scan, stack: &mut [u64], base: usize) -> Result<(u32, u64), Trap> {
        let slot = |index: u32| base + index as usize;
        let end = stack[slot(scan.end)] as u32;
        let start = stack[slot(scan.base)] as u32;
        let at = stack[slot(scan.at)] as u32;
        let table = match scan.test {
            Test::Table { table, .. } => self.memory.span(table, 0, BYTE_VALUES),
            Test::Equal(_) => Some(0..0),
        };
        let (Some(bytes), Some(table)) = (self.memory.span(start, at, end), table) else {
            return self.scan_bytewise(scan, stack, base);
        };
        let memory = &self.memory.bytes;
        let bytes = &memory[bytes];
        let table = &memory[table];
        let hit = |byte: u8| match scan.test {
            Test::Equal(value) => u32::from(byte) == value,
            Test::Table { .. } => table[usize::from(byte)] != 0,
        };
        let position = bytes.iter().position(|byte| hit(*byte));
        let iterations = position.unwrap_or(bytes.len());
        let last = position.or(iterations.checked_sub(1));
        if let Some(last) = last {
            let byte = bytes[last];
            stack[slot(scan.byte)] = u64::from(byte);
            if let Test::Table { class, .. } = scan.test {
                stack[slot(class)] = u64::from(table[usize::from(byte)]);
            }
        }
        stack[slot(scan.at)] = u64::from(at.wrapping_add(iterations as u32));
        let target = if position.is_some() {
            scan.found
        } else {
            scan.done
        };
        Ok((target, scan.ops(iterations as u64, position.is_some())))
    }

    fn scan_bytewise(
        &self,
        scan: Scan,
        stack: &mut [u64],
        base: usize,
    ) -> Result<(u32, u64), Trap> {
        let slot = |index: u32| base + index as usize;
        let end = stack[slot(scan.end)] as u32;
        let start = stack[slot(scan.base)] as u32;
        let mut at = stack[slot(scan.at)] as u32;
        let mut iterations: u64 = 0;
        let found = loop {
            if at >= end {
                break false;
            }
            let byte = self.memory.byte(u64::from(start.wrapping_add(at)))?;
            stack[slot(scan.byte)] = u64::from(byte);
            let hit = match scan.test {
                Test::Equal(value) => u32::from(byte) == value,
                Test::Table { class, table } => {
                    let entry = self
                        .memory
                        .byte(u64::from(u32::from(byte).wrapping_add(table)))?;
                    stack[slot(class)] = u64::from(entry);
                    entry != 0
                }
            };
            if hit {
                break true;
            }
            at = at.wrapping_add(1);
            stack[slot(scan.at)] = u64::from(at);
            iterations += 1;
        };
        let target = if found { scan.found } else { scan.done };
        Ok((target, scan.ops(iterations, found)))
    }

    fn crc(&self, crc: Crc, stack: &mut [u64], base: usize) -> Result<(u32, u64), Trap> {
        let slot = |index: u32| base + index as usize;
        let end = stack[slot(crc.end)] as u32;
        let start = stack[slot(crc.base)] as u32;
        let at = stack[slot(crc.at)] as u32;
        let table = self.memory.span(crc.table, 0, CRC_TABLE_BYTES);
        let (Some(bytes), Some(table)) = (self.memory.span(start, at, end), table) else {
            return self.crc_bytewise(crc, stack, base);
        };
        let memory = &self.memory.bytes;
        let table = &memory[table];
        let mut value = stack[slot(crc.crc)] as u32;
        let mut last = None;
        for byte in &memory[bytes.clone()] {
            let shifted = value >> Crc::BYTE_BITS;
            let byte = u32::from(*byte);
            let index = ((value ^ byte) & Crc::BYTE_MASK).wrapping_shl(Crc::WORD_SHIFT);
            let at = index as usize;
            let word = u32::from_le_bytes(
                table[at..at + CRC_WORD_BYTES]
                    .try_into()
                    .expect("a crc table word is four bytes"),
            );
            value = shifted ^ word;
            last = Some((shifted, byte, index, word));
        }
        if let Some((shifted, byte, index, word)) = last {
            stack[slot(crc.shifted)] = u64::from(shifted);
            stack[slot(crc.byte)] = u64::from(byte);
            stack[slot(crc.index)] = u64::from(index);
            stack[slot(crc.word)] = u64::from(word);
            stack[slot(crc.crc)] = u64::from(value);
            stack[slot(crc.at)] = u64::from(end);
        }
        Ok((crc.done, Crc::ops(bytes.len() as u64)))
    }

    fn crc_bytewise(&self, crc: Crc, stack: &mut [u64], base: usize) -> Result<(u32, u64), Trap> {
        let slot = |index: u32| base + index as usize;
        let end = stack[slot(crc.end)] as u32;
        let start = stack[slot(crc.base)] as u32;
        let mut at = stack[slot(crc.at)] as u32;
        let mut value = stack[slot(crc.crc)] as u32;
        let mut iterations: u64 = 0;
        while at < end {
            let shifted = value >> Crc::BYTE_BITS;
            stack[slot(crc.shifted)] = u64::from(shifted);
            let byte = u32::from(self.memory.byte(u64::from(start.wrapping_add(at)))?);
            stack[slot(crc.byte)] = u64::from(byte);
            let index = ((value ^ byte) & Crc::BYTE_MASK).wrapping_shl(Crc::WORD_SHIFT);
            stack[slot(crc.index)] = u64::from(index);
            let word =
                u32::from_le_bytes(self.memory.load(u64::from(index.wrapping_add(crc.table)))?);
            stack[slot(crc.word)] = u64::from(word);
            value = shifted ^ word;
            stack[slot(crc.crc)] = u64::from(value);
            at = at.wrapping_add(1);
            stack[slot(crc.at)] = u64::from(at);
            iterations += 1;
        }
        Ok((crc.done, Crc::ops(iterations)))
    }

    fn unhex(&mut self, unhex: Unhex, stack: &mut [u64], base: usize) -> Result<(u32, u64), Trap> {
        let slot = |index: u32| base + index as usize;
        let end = stack[slot(unhex.end)] as u32;
        let from = stack[slot(unhex.from)] as u32;
        let into = stack[slot(unhex.into)] as u32;
        let mut at = stack[slot(unhex.at)] as u32;
        let reads = end
            .checked_add(1)
            .and_then(|last| self.memory.span(from, at, last));
        let writes = self.memory.span(into, at >> 1, end.div_ceil(2));
        let values = self.memory.span(unhex.values, 0, BYTE_VALUES);
        let (Some(_), Some(_), Some(values)) = (reads, writes, values) else {
            return self.unhex_bytewise(unhex, stack, base);
        };
        let memory = &mut self.memory.bytes;
        let values = values.start;
        let mut pairs: u64 = 0;
        let mut last = None;
        let invalid = loop {
            if at >= end {
                break false;
            }
            let digits = (from + at) as usize;
            let second = memory[digits + 1];
            let high = u32::from(memory[values + usize::from(memory[digits])]);
            let low = u32::from(memory[values + usize::from(second)]);
            if (high | low) > Unhex::NIBBLE_MAX {
                last = Some((u32::from(second), high, low, high | low));
                break true;
            }
            let address = into + (at >> 1);
            let joined = (high << Unhex::NIBBLE_BITS) | low;
            memory[address as usize] = joined as u8;
            last = Some((joined, high, low, address));
            at += 2;
            pairs += 1;
        };
        if let Some((scratch, high, low, pair)) = last {
            stack[slot(unhex.scratch)] = u64::from(scratch);
            stack[slot(unhex.high)] = u64::from(high);
            stack[slot(unhex.low)] = u64::from(low);
            stack[slot(unhex.pair)] = u64::from(pair);
            stack[slot(unhex.at)] = u64::from(at);
        }
        let target = if invalid { unhex.invalid } else { unhex.done };
        Ok((target, Unhex::ops(pairs, invalid)))
    }

    fn unhex_bytewise(
        &mut self,
        unhex: Unhex,
        stack: &mut [u64],
        base: usize,
    ) -> Result<(u32, u64), Trap> {
        let slot = |index: u32| base + index as usize;
        let end = stack[slot(unhex.end)] as u32;
        let from = stack[slot(unhex.from)] as u32;
        let into = stack[slot(unhex.into)] as u32;
        let mut at = stack[slot(unhex.at)] as u32;
        let mut pairs: u64 = 0;
        let invalid = loop {
            if at >= end {
                break false;
            }
            let first = self.memory.byte(u64::from(from.wrapping_add(at)))?;
            let high = u32::from(
                self.memory
                    .byte(u64::from(u32::from(first).wrapping_add(unhex.values)))?,
            );
            stack[slot(unhex.scratch)] = u64::from(first);
            stack[slot(unhex.high)] = u64::from(high);
            let second = self.memory.byte(u64::from(from.wrapping_add(at)) + 1)?;
            stack[slot(unhex.scratch)] = u64::from(second);
            let low = u32::from(
                self.memory
                    .byte(u64::from(u32::from(second).wrapping_add(unhex.values)))?,
            );
            stack[slot(unhex.low)] = u64::from(low);
            stack[slot(unhex.pair)] = u64::from(high | low);
            if (high | low) > Unhex::NIBBLE_MAX {
                break true;
            }
            let address = into.wrapping_add(at >> 1);
            stack[slot(unhex.pair)] = u64::from(address);
            let joined = (high << Unhex::NIBBLE_BITS) | low;
            stack[slot(unhex.scratch)] = u64::from(joined);
            self.memory.store(u64::from(address), &[joined as u8])?;
            at = at.wrapping_add(2);
            stack[slot(unhex.at)] = u64::from(at);
            pairs += 1;
        };
        let target = if invalid { unhex.invalid } else { unhex.done };
        Ok((target, Unhex::ops(pairs, invalid)))
    }

    fn hex(&mut self, hex: Hex, stack: &mut [u64], base: usize) -> Result<(u32, u64), Trap> {
        let slot = |index: u32| base + index as usize;
        let end = stack[slot(hex.end)] as u32;
        let from = stack[slot(hex.from)] as u32;
        let into = stack[slot(hex.into)] as u32;
        let at = stack[slot(hex.at)] as u32;
        let reads = self.memory.span(from, at, end);
        let writes = end
            .checked_mul(2)
            .and_then(|last| self.memory.span(into, at.saturating_mul(2), last));
        let digits = self.memory.span(hex.digits, 0, HEX_DIGIT_COUNT);
        let (Some(reads), Some(_), Some(digits)) = (reads, writes, digits) else {
            return self.hex_bytewise(hex, stack, base);
        };
        if at >= end {
            return Ok((hex.done, Hex::ops(0)));
        }
        let memory = &mut self.memory.bytes;
        let digits = digits.start;
        let mut last = (0, 0, 0, 0);
        for (step, read) in reads.enumerate() {
            let byte = u32::from(memory[read]);
            let address = into + ((at + step as u32) << 1);
            let nibble = byte & Hex::NIBBLE_MASK;
            memory[address as usize] = memory[digits + (byte >> Hex::NIBBLE_BITS) as usize];
            let low = memory[digits + nibble as usize];
            memory[address as usize + 1] = low;
            last = (byte, u32::from(low), address, nibble);
        }
        let (byte, low, address, nibble) = last;
        stack[slot(hex.byte)] = u64::from(byte);
        stack[slot(hex.doubled)] = u64::from(low);
        stack[slot(hex.address)] = u64::from(address);
        stack[slot(hex.nibble)] = u64::from(nibble);
        stack[slot(hex.at)] = u64::from(end);
        Ok((hex.done, Hex::ops(u64::from(end - at))))
    }

    fn hex_bytewise(
        &mut self,
        hex: Hex,
        stack: &mut [u64],
        base: usize,
    ) -> Result<(u32, u64), Trap> {
        let slot = |index: u32| base + index as usize;
        let end = stack[slot(hex.end)] as u32;
        let from = stack[slot(hex.from)] as u32;
        let into = stack[slot(hex.into)] as u32;
        let mut at = stack[slot(hex.at)] as u32;
        let mut bytes: u64 = 0;
        while at < end {
            let byte = u32::from(self.memory.byte(u64::from(from.wrapping_add(at)))?);
            stack[slot(hex.byte)] = u64::from(byte);
            stack[slot(hex.doubled)] = u64::from(at << 1);
            let address = into.wrapping_add(at << 1);
            stack[slot(hex.address)] = u64::from(address);
            stack[slot(hex.nibble)] = u64::from(byte >> Hex::NIBBLE_BITS);
            let high = self.memory.byte(u64::from(
                (byte >> Hex::NIBBLE_BITS).wrapping_add(hex.digits),
            ))?;
            stack[slot(hex.doubled)] = u64::from(high);
            self.memory.store(u64::from(address), &[high])?;
            stack[slot(hex.doubled)] = u64::from(at << 1);
            let nibble = byte & Hex::NIBBLE_MASK;
            stack[slot(hex.nibble)] = u64::from(nibble);
            let low = self
                .memory
                .byte(u64::from(nibble.wrapping_add(hex.digits)))?;
            stack[slot(hex.doubled)] = u64::from(low);
            self.memory.store(u64::from(address) + 1, &[low])?;
            at = at.wrapping_add(1);
            stack[slot(hex.at)] = u64::from(at);
            bytes += 1;
        }
        Ok((hex.done, Hex::ops(bytes)))
    }

    fn words(&self, words: Words, stack: &mut [u64], base: usize) -> Result<(u32, u64), Trap> {
        let slot = |index: u32| base + index as usize;
        let end = stack[slot(words.end)] as u32;
        let left = stack[slot(words.left)] as u32;
        let right = stack[slot(words.right)] as u32;
        let at = stack[slot(words.at)] as u32;
        if at > end || end > u32::MAX - Words::BYTES {
            return self.words_bytewise(words, stack, base);
        }
        let count = (end - at) / Words::BYTES;
        let last = at + count * Words::BYTES;
        let (Some(lefts), Some(rights)) = (
            self.memory.span(left, at, last),
            self.memory.span(right, at, last),
        ) else {
            return self.words_bytewise(words, stack, base);
        };
        let memory = &self.memory.bytes;
        let differ = if memory[lefts.clone()] == memory[rights.clone()] {
            None
        } else {
            let (lefts, _) = memory[lefts].as_chunks::<{ Words::BYTES as usize }>();
            let (rights, _) = memory[rights].as_chunks::<{ Words::BYTES as usize }>();
            lefts
                .iter()
                .zip(rights)
                .position(|(left, right)| left != right)
        };
        let equal = differ.map_or(count, |index| index as u32);
        let word = |base: u32, at: u32| {
            let start = (base + at) as usize;
            u64::from_le_bytes(
                memory[start..start + Words::BYTES as usize]
                    .try_into()
                    .expect("a word is eight bytes"),
            )
        };
        if equal > 0 {
            let previous = at + (equal - 1) * Words::BYTES;
            stack[slot(words.next)] = u64::from(previous + Words::BYTES);
            stack[slot(words.left_word)] = word(left, previous);
            stack[slot(words.right_word)] = word(right, previous);
            stack[slot(words.differ)] = 0;
        }
        let current = at + equal * Words::BYTES;
        stack[slot(words.at)] = u64::from(current);
        stack[slot(words.next)] = u64::from(current + Words::BYTES);
        if differ.is_some() {
            stack[slot(words.left_word)] = word(left, current);
            stack[slot(words.right_word)] = word(right, current);
            stack[slot(words.differ)] = 1;
        }
        Ok((words.done, Words::ops(u64::from(equal), differ.is_some())))
    }

    fn words_bytewise(
        &self,
        words: Words,
        stack: &mut [u64],
        base: usize,
    ) -> Result<(u32, u64), Trap> {
        let slot = |index: u32| base + index as usize;
        let end = stack[slot(words.end)] as u32;
        let left = stack[slot(words.left)] as u32;
        let right = stack[slot(words.right)] as u32;
        let mut at = stack[slot(words.at)] as u32;
        let mut equal: u64 = 0;
        loop {
            let next = at.wrapping_add(Words::BYTES);
            stack[slot(words.next)] = u64::from(next);
            if next > end {
                return Ok((words.done, Words::ops(equal, false)));
            }
            let left_word = self.load(Load::I64, u64::from(left.wrapping_add(at)))?;
            stack[slot(words.left_word)] = left_word;
            let right_word = self.load(Load::I64, u64::from(right.wrapping_add(at)))?;
            stack[slot(words.right_word)] = right_word;
            let differ = left_word != right_word;
            stack[slot(words.differ)] = u64::from(differ);
            if differ {
                return Ok((words.done, Words::ops(equal, true)));
            }
            at = at.wrapping_add(Words::BYTES);
            stack[slot(words.at)] = u64::from(at);
            equal += 1;
        }
    }

    fn other(&mut self, inst: Inst, stack: &mut [u64], base: usize) -> Result<(), Trap> {
        macro_rules! slot {
            ($index:expr) => {
                stack[base + $index as usize]
            };
        }
        match inst {
            Inst::Unreachable => return Err(Trap::Unreachable),
            Inst::Select {
                result,
                first,
                second,
                condition,
            } => {
                slot!(result) = if slot!(condition) as u32 != 0 {
                    slot!(first)
                } else {
                    slot!(second)
                };
            }
            Inst::MemorySize { result } => slot!(result) = u64::from(self.memory.pages()),
            Inst::MemoryGrow { result, delta } => {
                slot!(result) = u64::from(self.memory.grow(slot!(delta) as u32));
            }
            Inst::MemoryCopy {
                destination,
                source,
                length,
            } => {
                let length = u64::from(slot!(length) as u32);
                let to = self
                    .memory
                    .range(u64::from(slot!(destination) as u32), length)?;
                let from = self.memory.range(u64::from(slot!(source) as u32), length)?;
                self.memory.bytes.copy_within(from, to.start);
            }
            Inst::MemoryFill {
                destination,
                value,
                length,
            } => {
                let to = self.memory.range(
                    u64::from(slot!(destination) as u32),
                    u64::from(slot!(length) as u32),
                )?;
                self.memory.bytes[to].fill(slot!(value) as u8);
            }
            Inst::I32Unary {
                unary,
                result,
                source,
            } => slot!(result) = u64::from(i32_unary(unary, slot!(source) as u32)),
            Inst::I64Unary {
                unary,
                result,
                source,
            } => slot!(result) = i64_unary(unary, slot!(source)),
            Inst::I32Binary {
                binary,
                result,
                left,
                right,
            } => {
                let value = i32_binary(binary, slot!(left) as u32, slot!(right) as u32)?;
                slot!(result) = u64::from(value);
            }
            Inst::I32BinaryImmediate {
                binary,
                result,
                left,
                right,
            } => slot!(result) = u64::from(i32_binary(binary, slot!(left) as u32, right)?),
            Inst::I64Binary {
                binary,
                result,
                left,
                right,
            } => slot!(result) = i64_binary(binary, slot!(left), slot!(right))?,
            Inst::I64BinaryConstant {
                binary,
                result,
                left,
                right,
            } => slot!(result) = i64_binary(binary, slot!(left), right)?,
            _ => unreachable!("run handles {inst:?} itself"),
        }
        Ok(())
    }

    fn load(&self, load: Load, address: u64) -> Result<u64, Trap> {
        let memory = &self.memory;
        Ok(match load {
            Load::I32 => u64::from(u32::from_le_bytes(memory.load(address)?)),
            Load::I64 => u64::from_le_bytes(memory.load(address)?),
            Load::I32S8 => u64::from(i32::from(i8::from_le_bytes(memory.load(address)?)) as u32),
            Load::I32U8 | Load::I64U8 => u64::from(memory.byte(address)?),
            Load::I32S16 => u64::from(i32::from(i16::from_le_bytes(memory.load(address)?)) as u32),
            Load::I32U16 | Load::I64U16 => u64::from(u16::from_le_bytes(memory.load(address)?)),
            Load::I64S8 => i64::from(i8::from_le_bytes(memory.load(address)?)) as u64,
            Load::I64S16 => i64::from(i16::from_le_bytes(memory.load(address)?)) as u64,
            Load::I64S32 => i64::from(i32::from_le_bytes(memory.load(address)?)) as u64,
            Load::I64U32 => u64::from(u32::from_le_bytes(memory.load(address)?)),
        })
    }

    fn store(&mut self, store: Store, address: u64, value: u64) -> Result<(), Trap> {
        let memory = &mut self.memory;
        match store {
            Store::I32 | Store::I64As32 => memory.store(address, &(value as u32).to_le_bytes()),
            Store::I64 => memory.store(address, &value.to_le_bytes()),
            Store::I32As8 | Store::I64As8 => memory.store(address, &[value as u8]),
            Store::I32As16 | Store::I64As16 => memory.store(address, &(value as u16).to_le_bytes()),
        }
    }
}

fn i32_unary(op: Unary, value: u32) -> u32 {
    match op {
        Unary::Eqz => u32::from(value == 0),
        Unary::Clz => value.leading_zeros(),
        Unary::Ctz => value.trailing_zeros(),
        Unary::Popcnt => value.count_ones(),
    }
}

fn i64_unary(op: Unary, value: u64) -> u64 {
    match op {
        Unary::Eqz => u64::from(value == 0),
        Unary::Clz => u64::from(value.leading_zeros()),
        Unary::Ctz => u64::from(value.trailing_zeros()),
        Unary::Popcnt => u64::from(value.count_ones()),
    }
}

pub(super) fn i32_binary(op: Binary, left: u32, right: u32) -> Result<u32, Trap> {
    let (signed_left, signed_right) = (left as i32, right as i32);
    Ok(match op {
        Binary::Eq => u32::from(left == right),
        Binary::Ne => u32::from(left != right),
        Binary::LtS => u32::from(signed_left < signed_right),
        Binary::LtU => u32::from(left < right),
        Binary::GtS => u32::from(signed_left > signed_right),
        Binary::GtU => u32::from(left > right),
        Binary::LeS => u32::from(signed_left <= signed_right),
        Binary::LeU => u32::from(left <= right),
        Binary::GeS => u32::from(signed_left >= signed_right),
        Binary::GeU => u32::from(left >= right),
        Binary::Add => left.wrapping_add(right),
        Binary::Sub => left.wrapping_sub(right),
        Binary::Mul => left.wrapping_mul(right),
        Binary::DivS => {
            signed_div(i64::from(signed_left), i64::from(signed_right), I32_BITS)? as u32
        }
        Binary::DivU => left.checked_div(right).ok_or(Trap::DivideByZero)?,
        Binary::RemS => signed_rem(i64::from(signed_left), i64::from(signed_right))? as u32,
        Binary::RemU => left.checked_rem(right).ok_or(Trap::DivideByZero)?,
        Binary::And => left & right,
        Binary::Or => left | right,
        Binary::Xor => left ^ right,
        Binary::Shl => left.wrapping_shl(right),
        Binary::ShrS => signed_left.wrapping_shr(right) as u32,
        Binary::ShrU => left.wrapping_shr(right),
        Binary::Rotl => left.rotate_left(right % I32_BITS),
        Binary::Rotr => left.rotate_right(right % I32_BITS),
    })
}

pub(super) fn i64_binary(op: Binary, left: u64, right: u64) -> Result<u64, Trap> {
    let (signed_left, signed_right) = (left as i64, right as i64);
    let shift = right as u32;
    Ok(match op {
        Binary::Eq => u64::from(left == right),
        Binary::Ne => u64::from(left != right),
        Binary::LtS => u64::from(signed_left < signed_right),
        Binary::LtU => u64::from(left < right),
        Binary::GtS => u64::from(signed_left > signed_right),
        Binary::GtU => u64::from(left > right),
        Binary::LeS => u64::from(signed_left <= signed_right),
        Binary::LeU => u64::from(left <= right),
        Binary::GeS => u64::from(signed_left >= signed_right),
        Binary::GeU => u64::from(left >= right),
        Binary::Add => left.wrapping_add(right),
        Binary::Sub => left.wrapping_sub(right),
        Binary::Mul => left.wrapping_mul(right),
        Binary::DivS => signed_div(signed_left, signed_right, I64_BITS)? as u64,
        Binary::DivU => left.checked_div(right).ok_or(Trap::DivideByZero)?,
        Binary::RemS => signed_rem(signed_left, signed_right)? as u64,
        Binary::RemU => left.checked_rem(right).ok_or(Trap::DivideByZero)?,
        Binary::And => left & right,
        Binary::Or => left | right,
        Binary::Xor => left ^ right,
        Binary::Shl => left.wrapping_shl(shift),
        Binary::ShrS => signed_left.wrapping_shr(shift) as u64,
        Binary::ShrU => left.wrapping_shr(shift),
        Binary::Rotl => left.rotate_left(shift % I64_BITS),
        Binary::Rotr => left.rotate_right(shift % I64_BITS),
    })
}

fn signed_div(left: i64, right: i64, bits: u32) -> Result<i64, Trap> {
    if right == 0 {
        return Err(Trap::DivideByZero);
    }
    let minimum = if bits == I64_BITS {
        i64::MIN
    } else {
        -(1_i64 << (bits - 1))
    };
    if left == minimum && right == -1 {
        return Err(Trap::IntegerOverflow);
    }
    Ok(left / right)
}

fn signed_rem(left: i64, right: i64) -> Result<i64, Trap> {
    if right == 0 {
        return Err(Trap::DivideByZero);
    }
    Ok(left.wrapping_rem(right))
}
