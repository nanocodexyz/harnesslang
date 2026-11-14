pub const MAGIC: [u8; 4] = *b"\0asm";
pub const VERSION: [u8; 4] = [1, 0, 0, 0];
pub const PAGE_BYTES: u32 = 65536;

const SECTION_TYPE: u8 = 1;
const SECTION_IMPORT: u8 = 2;
const SECTION_FUNCTION: u8 = 3;
const SECTION_MEMORY: u8 = 5;
const SECTION_GLOBAL: u8 = 6;
const SECTION_EXPORT: u8 = 7;
const SECTION_CODE: u8 = 10;
const SECTION_DATA: u8 = 11;
const FUNCTION_TYPE: u8 = 0x60;
const KIND_FUNCTION: u8 = 0;
const KIND_MEMORY: u8 = 2;
const KIND_GLOBAL: u8 = 3;
const LIMITS_MIN_ONLY: u8 = 0;
const GLOBAL_CONST: u8 = 0;
const GLOBAL_MUT: u8 = 1;
const DATA_ACTIVE_MEMORY_ZERO: u8 = 0;
const LEB_PAYLOAD_BITS: u32 = 7;
const LEB_PAYLOAD_MASK: u64 = 0x7f;
const LEB_CONTINUE: u8 = 0x80;
const LEB_SIGN: u8 = 0x40;
pub const OP_END: u8 = 0x0b;
pub const OP_I32_CONST: u8 = 0x41;
pub const OP_I64_CONST: u8 = 0x42;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ValType {
    I32,
    I64,
}

impl ValType {
    pub const fn code(self) -> u8 {
        match self {
            Self::I32 => 0x7f,
            Self::I64 => 0x7e,
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "i32" => Some(Self::I32),
            "i64" => Some(Self::I64),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FuncType {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub module: String,
    pub name: String,
    pub ty: u32,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub ty: u32,
    pub locals: Vec<ValType>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct Global {
    pub ty: ValType,
    pub mutable: bool,
    pub init: i64,
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

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub types: Vec<FuncType>,
    pub imports: Vec<Import>,
    pub functions: Vec<Function>,
    pub memory_pages: Option<u32>,
    pub globals: Vec<Global>,
    pub exports: Vec<Export>,
    pub data: Vec<Data>,
}

impl Module {
    pub fn type_index(&mut self, ty: FuncType) -> u32 {
        if let Some(index) = self.types.iter().position(|known| *known == ty) {
            return index_u32(index);
        }
        self.types.push(ty);
        index_u32(self.types.len() - 1)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION);
        section(&mut out, SECTION_TYPE, &self.type_section());
        section(&mut out, SECTION_IMPORT, &self.import_section());
        section(&mut out, SECTION_FUNCTION, &self.function_section());
        if let Some(pages) = self.memory_pages {
            let mut memory = Vec::new();
            count(&mut memory, 1);
            memory.push(LIMITS_MIN_ONLY);
            unsigned(&mut memory, u64::from(pages));
            section(&mut out, SECTION_MEMORY, &memory);
        }
        section(&mut out, SECTION_GLOBAL, &self.global_section());
        section(&mut out, SECTION_EXPORT, &self.export_section());
        section(&mut out, SECTION_CODE, &self.code_section());
        section(&mut out, SECTION_DATA, &self.data_section());
        out
    }

    fn type_section(&self) -> Vec<u8> {
        let mut body = Vec::new();
        count(&mut body, self.types.len());
        for ty in &self.types {
            body.push(FUNCTION_TYPE);
            count(&mut body, ty.params.len());
            body.extend(ty.params.iter().map(|param| param.code()));
            count(&mut body, ty.results.len());
            body.extend(ty.results.iter().map(|result| result.code()));
        }
        body
    }

    fn import_section(&self) -> Vec<u8> {
        let mut body = Vec::new();
        count(&mut body, self.imports.len());
        for import in &self.imports {
            name(&mut body, &import.module);
            name(&mut body, &import.name);
            body.push(KIND_FUNCTION);
            unsigned(&mut body, u64::from(import.ty));
        }
        body
    }

    fn function_section(&self) -> Vec<u8> {
        let mut body = Vec::new();
        count(&mut body, self.functions.len());
        for function in &self.functions {
            unsigned(&mut body, u64::from(function.ty));
        }
        body
    }

    fn global_section(&self) -> Vec<u8> {
        let mut body = Vec::new();
        count(&mut body, self.globals.len());
        for global in &self.globals {
            body.push(global.ty.code());
            body.push(if global.mutable { GLOBAL_MUT } else { GLOBAL_CONST });
            body.push(match global.ty {
                ValType::I32 => OP_I32_CONST,
                ValType::I64 => OP_I64_CONST,
            });
            signed(&mut body, global.init);
            body.push(OP_END);
        }
        body
    }

    fn export_section(&self) -> Vec<u8> {
        let mut body = Vec::new();
        count(&mut body, self.exports.len());
        for export in &self.exports {
            name(&mut body, &export.name);
            body.push(match export.kind {
                ExportKind::Function => KIND_FUNCTION,
                ExportKind::Memory => KIND_MEMORY,
                ExportKind::Global => KIND_GLOBAL,
            });
            unsigned(&mut body, u64::from(export.index));
        }
        body
    }

    fn code_section(&self) -> Vec<u8> {
        let mut body = Vec::new();
        count(&mut body, self.functions.len());
        for function in &self.functions {
            let mut code = Vec::new();
            let groups = local_groups(&function.locals);
            count(&mut code, groups.len());
            for (ty, run) in groups {
                count(&mut code, run);
                code.push(ty.code());
            }
            assert_eq!(function.body.last(), Some(&OP_END), "a function body ends with end");
            code.extend_from_slice(&function.body);
            count(&mut body, code.len());
            body.extend_from_slice(&code);
        }
        body
    }

    fn data_section(&self) -> Vec<u8> {
        let mut body = Vec::new();
        count(&mut body, self.data.len());
        for data in &self.data {
            body.push(DATA_ACTIVE_MEMORY_ZERO);
            body.push(OP_I32_CONST);
            signed(&mut body, i64::from(data.offset as i32));
            body.push(OP_END);
            count(&mut body, data.bytes.len());
            body.extend_from_slice(&data.bytes);
        }
        body
    }
}

fn local_groups(locals: &[ValType]) -> Vec<(ValType, usize)> {
    let mut groups: Vec<(ValType, usize)> = Vec::new();
    for local in locals {
        match groups.last_mut() {
            Some((ty, run)) if ty == local => *run += 1,
            _ => groups.push((*local, 1)),
        }
    }
    groups
}

fn section(out: &mut Vec<u8>, id: u8, body: &[u8]) {
    out.push(id);
    count(out, body.len());
    out.extend_from_slice(body);
}

fn name(out: &mut Vec<u8>, text: &str) {
    count(out, text.len());
    out.extend_from_slice(text.as_bytes());
}

pub fn index_u32(index: usize) -> u32 {
    u32::try_from(index).expect("a wasm index fits u32")
}

pub fn count(out: &mut Vec<u8>, value: usize) {
    unsigned(out, u64::try_from(value).expect("a count fits u64"));
}

pub fn unsigned(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & LEB_PAYLOAD_MASK) as u8;
        value >>= LEB_PAYLOAD_BITS;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | LEB_CONTINUE);
    }
}

pub fn signed(out: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = (value as u64 & LEB_PAYLOAD_MASK) as u8;
        value >>= LEB_PAYLOAD_BITS;
        let done = (value == 0 && byte & LEB_SIGN == 0) || (value == -1 && byte & LEB_SIGN != 0);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | LEB_CONTINUE);
    }
}
