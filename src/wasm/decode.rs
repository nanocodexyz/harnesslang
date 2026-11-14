use super::ValType;
use super::compile::{self, Code};
use crate::emit::wasm::encode::{MAGIC, PAGE_BYTES, VERSION};

pub const MODULE_BYTES_MAX: usize = 1 << 30;
pub const MEMORY_PAGES_MAX: u32 = 1 << 16;
pub const FUNCTION_LOCALS_MAX: u32 = 50_000;
pub const FUNCTION_PARAMS_MAX: usize = 1_000;
pub const BR_TABLE_LABELS_MAX: u32 = 65_520;
const LEB_PAYLOAD_BITS: u32 = 7;
const LEB_CONTINUE: u8 = 0x80;
const LEB_PAYLOAD_MASK: u8 = 0x7f;
const LEB_SIGN: u8 = 0x40;
const SECTION_TYPE: u8 = 1;
const SECTION_IMPORT: u8 = 2;
const SECTION_FUNCTION: u8 = 3;
const SECTION_MEMORY: u8 = 5;
const SECTION_GLOBAL: u8 = 6;
const SECTION_EXPORT: u8 = 7;
const SECTION_CODE: u8 = 10;
const SECTION_DATA: u8 = 11;
const SECTION_DATA_COUNT: u8 = 12;
const SECTION_ORDER: [u8; 9] = [
    SECTION_TYPE,
    SECTION_IMPORT,
    SECTION_FUNCTION,
    SECTION_MEMORY,
    SECTION_GLOBAL,
    SECTION_EXPORT,
    SECTION_DATA_COUNT,
    SECTION_CODE,
    SECTION_DATA,
];
const FUNCTION_TYPE: u8 = 0x60;
const TYPE_I32: u8 = 0x7f;
const TYPE_I64: u8 = 0x7e;
const KIND_FUNCTION: u8 = 0;
const KIND_MEMORY: u8 = 2;
const KIND_GLOBAL: u8 = 3;
const LIMITS_MIN: u8 = 0;
const LIMITS_MIN_MAX: u8 = 1;
const GLOBAL_CONST: u8 = 0;
const GLOBAL_MUT: u8 = 1;
const DATA_ACTIVE_MEMORY_ZERO: u32 = 0;
const OP_END: u8 = 0x0b;
const OP_I32_CONST: u8 = 0x41;
const OP_I64_CONST: u8 = 0x42;
const RESULTS_MAX: usize = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuncType {
    pub params: Vec<ValType>,
    pub result: Option<ValType>,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub module: String,
    pub name: String,
    pub ty: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Global {
    pub ty: ValType,
    pub mutable: bool,
    pub init: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    Function,
    Memory,
    Global,
}

#[derive(Clone, Debug)]
pub struct Export {
    pub name: String,
    pub kind: ExportKind,
    pub index: u32,
}

#[derive(Clone, Debug)]
pub struct Data {
    pub offset: u32,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub min: u32,
    pub max: Option<u32>,
}

#[derive(Debug, Default)]
pub struct Module {
    pub types: Vec<FuncType>,
    pub imports: Vec<Import>,
    pub function_types: Vec<u32>,
    pub memory: Option<Limits>,
    pub globals: Vec<Global>,
    pub exports: Vec<Export>,
    pub data: Vec<Data>,
    pub code: Vec<Code>,
}

pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    base: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8], base: usize) -> Self {
        Reader { bytes, at: 0, base }
    }

    pub fn fail<T>(&self, message: impl std::fmt::Display) -> Result<T, String> {
        Err(format!("wasm byte {}: {message}", self.base + self.at))
    }

    pub fn done(&self) -> bool {
        self.at == self.bytes.len()
    }

    pub fn offset(&self) -> usize {
        self.base + self.at
    }

    pub fn byte(&mut self) -> Result<u8, String> {
        match self.bytes.get(self.at) {
            Some(&byte) => {
                self.at += 1;
                Ok(byte)
            }
            None => self.fail("unexpected end"),
        }
    }

    pub fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(length);
        match end.and_then(|end| self.bytes.get(self.at..end)) {
            Some(slice) => {
                self.at += length;
                Ok(slice)
            }
            None => self.fail(format!("{length} bytes run past the end")),
        }
    }

    fn unsigned(&mut self, bits: u32) -> Result<u64, String> {
        let bytes_max = bits.div_ceil(LEB_PAYLOAD_BITS);
        let mut value: u128 = 0;
        for index in 0..bytes_max {
            let byte = self.byte()?;
            value |= u128::from(byte & LEB_PAYLOAD_MASK) << (index * LEB_PAYLOAD_BITS);
            if byte & LEB_CONTINUE == 0 {
                if value >> bits != 0 {
                    return self.fail(format!("an unsigned LEB128 does not fit {bits} bits"));
                }
                return Ok(value as u64);
            }
        }
        self.fail(format!("an unsigned LEB128 is longer than {bytes_max} bytes"))
    }

    fn signed(&mut self, bits: u32) -> Result<i64, String> {
        let bytes_max = bits.div_ceil(LEB_PAYLOAD_BITS);
        let mut value: i128 = 0;
        for index in 0..bytes_max {
            let byte = self.byte()?;
            let shift = index * LEB_PAYLOAD_BITS;
            value |= i128::from(byte & LEB_PAYLOAD_MASK) << shift;
            if byte & LEB_CONTINUE == 0 {
                if byte & LEB_SIGN != 0 {
                    value |= -1_i128 << (shift + LEB_PAYLOAD_BITS);
                }
                let low = -(1_i128 << (bits - 1));
                let high = (1_i128 << (bits - 1)) - 1;
                if value < low || value > high {
                    return self.fail(format!("a signed LEB128 does not fit {bits} bits"));
                }
                return Ok(value as i64);
            }
        }
        self.fail(format!("a signed LEB128 is longer than {bytes_max} bytes"))
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        self.unsigned(u32::BITS).map(|value| value as u32)
    }

    pub fn s32(&mut self) -> Result<i32, String> {
        self.signed(i32::BITS).map(|value| value as i32)
    }

    pub fn s64(&mut self) -> Result<i64, String> {
        self.signed(i64::BITS)
    }

    pub fn count(&mut self, what: &str) -> Result<usize, String> {
        let count = self.u32()? as usize;
        if count > self.bytes.len() - self.at {
            return self.fail(format!("{count} {what} cannot fit in the bytes left"));
        }
        Ok(count)
    }

    pub fn value_type(&mut self) -> Result<ValType, String> {
        match self.byte()? {
            TYPE_I32 => Ok(ValType::I32),
            TYPE_I64 => Ok(ValType::I64),
            other => self.fail(format!("value type 0x{other:02x} is not supported (only i32 and i64)")),
        }
    }

    fn name(&mut self) -> Result<String, String> {
        let length = self.count("name bytes")?;
        let bytes = self.take(length)?;
        match std::str::from_utf8(bytes) {
            Ok(text) => Ok(text.to_owned()),
            Err(_) => self.fail("a name is not UTF-8"),
        }
    }
}

impl Module {
    pub fn decode(bytes: &[u8]) -> Result<Module, String> {
        if bytes.len() > MODULE_BYTES_MAX {
            return Err(format!("wasm module of {} bytes is over module_bytes_max ({MODULE_BYTES_MAX})", bytes.len()));
        }
        let mut reader = Reader::new(bytes, 0);
        if reader.take(MAGIC.len())? != MAGIC {
            return reader.fail("not a wasm module (bad magic)");
        }
        if reader.take(VERSION.len())? != VERSION {
            return reader.fail("unsupported wasm version");
        }
        let mut module = Module::default();
        let mut data_count = None;
        let mut last = None;
        while !reader.done() {
            let id = reader.byte()?;
            let Some(order) = SECTION_ORDER.iter().position(|known| *known == id) else {
                return reader.fail(format!("section id {id} is not supported"));
            };
            if last.is_some_and(|last| order <= last) {
                return reader.fail(format!("section id {id} is out of order or repeated"));
            }
            last = Some(order);
            let length = reader.count("section bytes")?;
            let base = reader.offset();
            let mut section = Reader::new(reader.take(length)?, base);
            module.section(id, &mut section, &mut data_count)?;
            if !section.done() {
                return section.fail(format!("section id {id} has trailing bytes"));
            }
        }
        module.finish(data_count, &reader)?;
        Ok(module)
    }

    fn section(&mut self, id: u8, reader: &mut Reader, data_count: &mut Option<u32>) -> Result<(), String> {
        match id {
            SECTION_TYPE => self.types(reader),
            SECTION_IMPORT => self.imports(reader),
            SECTION_FUNCTION => {
                let count = reader.count("functions")?;
                for _ in 0..count {
                    let ty = reader.u32()?;
                    if ty as usize >= self.types.len() {
                        return reader.fail(format!("function type {ty} does not exist"));
                    }
                    self.function_types.push(ty);
                }
                Ok(())
            }
            SECTION_MEMORY => self.memory(reader),
            SECTION_GLOBAL => self.globals(reader),
            SECTION_EXPORT => self.exports(reader),
            SECTION_DATA_COUNT => {
                *data_count = Some(reader.u32()?);
                Ok(())
            }
            SECTION_CODE => self.code(reader),
            SECTION_DATA => self.data(reader),
            _ => unreachable!("section ids are checked against section_order"),
        }
    }

    fn types(&mut self, reader: &mut Reader) -> Result<(), String> {
        let count = reader.count("types")?;
        for _ in 0..count {
            if reader.byte()? != FUNCTION_TYPE {
                return reader.fail("only function types are supported");
            }
            let params_count = reader.count("params")?;
            if params_count > FUNCTION_PARAMS_MAX {
                return reader.fail(format!("{params_count} params are over function_params_max ({FUNCTION_PARAMS_MAX})"));
            }
            let mut params = Vec::with_capacity(params_count);
            for _ in 0..params_count {
                params.push(reader.value_type()?);
            }
            let results_count = reader.count("results")?;
            if results_count > RESULTS_MAX {
                return reader.fail("multi-value results are not supported");
            }
            let result = if results_count == 1 { Some(reader.value_type()?) } else { None };
            self.types.push(FuncType { params, result });
        }
        Ok(())
    }

    fn imports(&mut self, reader: &mut Reader) -> Result<(), String> {
        let count = reader.count("imports")?;
        for _ in 0..count {
            let module = reader.name()?;
            let name = reader.name()?;
            if reader.byte()? != KIND_FUNCTION {
                return reader.fail(format!("import {module}.{name} is not a function; only function imports are supported"));
            }
            let ty = reader.u32()?;
            if ty as usize >= self.types.len() {
                return reader.fail(format!("import type {ty} does not exist"));
            }
            self.imports.push(Import { module, name, ty });
        }
        Ok(())
    }

    fn memory(&mut self, reader: &mut Reader) -> Result<(), String> {
        if reader.count("memories")? != 1 {
            return reader.fail("exactly one memory is supported");
        }
        let flag = reader.byte()?;
        let min = reader.u32()?;
        let max = match flag {
            LIMITS_MIN => None,
            LIMITS_MIN_MAX => Some(reader.u32()?),
            _ => return reader.fail(format!("memory limits flag {flag} is not supported")),
        };
        if min > MEMORY_PAGES_MAX || max.is_some_and(|max| max > MEMORY_PAGES_MAX || max < min) {
            return reader.fail("memory limits are invalid");
        }
        self.memory = Some(Limits { min, max });
        Ok(())
    }

    fn globals(&mut self, reader: &mut Reader) -> Result<(), String> {
        let count = reader.count("globals")?;
        for _ in 0..count {
            let ty = reader.value_type()?;
            let mutable = match reader.byte()? {
                GLOBAL_CONST => false,
                GLOBAL_MUT => true,
                other => return reader.fail(format!("global mutability {other} is invalid")),
            };
            let init = constant(reader, ty)?;
            self.globals.push(Global { ty, mutable, init });
        }
        Ok(())
    }

    fn exports(&mut self, reader: &mut Reader) -> Result<(), String> {
        let count = reader.count("exports")?;
        for _ in 0..count {
            let name = reader.name()?;
            let kind = match reader.byte()? {
                KIND_FUNCTION => ExportKind::Function,
                KIND_MEMORY => ExportKind::Memory,
                KIND_GLOBAL => ExportKind::Global,
                other => return reader.fail(format!("export kind {other} is not supported")),
            };
            let index = reader.u32()?;
            if self.exports.iter().any(|export| export.name == name) {
                return reader.fail(format!("export {name} is repeated"));
            }
            self.exports.push(Export { name, kind, index });
        }
        Ok(())
    }

    fn code(&mut self, reader: &mut Reader) -> Result<(), String> {
        let count = reader.count("function bodies")?;
        if count != self.function_types.len() {
            return reader.fail(format!("{count} function bodies for {} functions", self.function_types.len()));
        }
        for index in 0..count {
            let length = reader.count("body bytes")?;
            let base = reader.offset();
            let mut body = Reader::new(reader.take(length)?, base);
            let code = compile::compile(self, index, &mut body)?;
            self.code.push(code);
        }
        Ok(())
    }

    fn data(&mut self, reader: &mut Reader) -> Result<(), String> {
        let count = reader.count("data segments")?;
        for _ in 0..count {
            let flag = reader.u32()?;
            if flag != DATA_ACTIVE_MEMORY_ZERO {
                return reader.fail(format!("data segment flag {flag} is not supported (only active segments in memory 0)"));
            }
            let offset = constant(reader, ValType::I32)? as u32;
            let length = reader.count("data bytes")?;
            let bytes = reader.take(length)?.to_vec();
            let end = u64::from(offset) + bytes.len() as u64;
            let available = self.memory.map_or(0, |memory| u64::from(memory.min) * u64::from(PAGE_BYTES));
            if end > available {
                return reader.fail(format!("a data segment ends at byte {end}, past the initial memory of {available} bytes"));
            }
            self.data.push(Data { offset, bytes });
        }
        Ok(())
    }

    fn finish(&self, data_count: Option<u32>, reader: &Reader) -> Result<(), String> {
        if self.code.len() != self.function_types.len() {
            return reader.fail("the code section is missing");
        }
        if let Some(count) = data_count
            && count as usize != self.data.len()
        {
            return reader.fail(format!("data count {count} does not match {} data segments", self.data.len()));
        }
        let functions = self.imports.len() + self.function_types.len();
        for export in &self.exports {
            let bound = match export.kind {
                ExportKind::Function => functions,
                ExportKind::Memory => usize::from(self.memory.is_some()),
                ExportKind::Global => self.globals.len(),
            };
            if export.index as usize >= bound {
                return reader.fail(format!("export {} names index {}, which does not exist", export.name, export.index));
            }
        }
        Ok(())
    }

    pub fn function_type(&self, function: u32) -> &FuncType {
        let index = function as usize;
        let ty = match index.checked_sub(self.imports.len()) {
            None => self.imports[index].ty,
            Some(local) => self.function_types[local],
        };
        &self.types[ty as usize]
    }

    pub fn export_function(&self, name: &str) -> Option<u32> {
        self.exports
            .iter()
            .find(|export| export.kind == ExportKind::Function && export.name == name)
            .map(|export| export.index)
    }
}

fn constant(reader: &mut Reader, ty: ValType) -> Result<u64, String> {
    let value = match (reader.byte()?, ty) {
        (OP_I32_CONST, ValType::I32) => u64::from(reader.s32()? as u32),
        (OP_I64_CONST, ValType::I64) => reader.s64()? as u64,
        _ => return reader.fail(format!("a constant expression is only {}.const", ty.name())),
    };
    if reader.byte()? != OP_END {
        return reader.fail("a constant expression ends after one constant");
    }
    Ok(value)
}
