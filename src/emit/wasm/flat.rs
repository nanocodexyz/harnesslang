use super::encode::{ExportKind, Module, ValType};

fn word(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn length(out: &mut Vec<u8>, value: usize) {
    word(out, u32::try_from(value).expect("a flat length fits u32"));
}

fn codes(out: &mut Vec<u8>, types: &[ValType]) {
    length(out, types.len());
    out.extend(types.iter().map(|ty| ty.code()));
}

fn text(out: &mut Vec<u8>, value: &[u8]) {
    length(out, value.len());
    out.extend_from_slice(value);
}

pub fn flatten(module: &Module) -> Vec<u8> {
    let mut out = Vec::new();
    length(&mut out, module.types.len());
    for ty in &module.types {
        codes(&mut out, &ty.params);
        codes(&mut out, &ty.results);
    }
    length(&mut out, module.imports.len());
    for import in &module.imports {
        text(&mut out, import.module.as_bytes());
        text(&mut out, import.name.as_bytes());
        word(&mut out, import.ty);
    }
    length(&mut out, module.functions.len());
    for function in &module.functions {
        word(&mut out, function.ty);
        codes(&mut out, &function.locals);
        text(&mut out, &function.body);
    }
    match module.memory_pages {
        Some(pages) => {
            word(&mut out, 1);
            word(&mut out, pages);
        }
        None => {
            word(&mut out, 0);
            word(&mut out, 0);
        }
    }
    length(&mut out, module.globals.len());
    for global in &module.globals {
        out.push(global.ty.code());
        out.push(u8::from(global.mutable));
        out.extend_from_slice(&global.init.to_le_bytes());
    }
    length(&mut out, module.exports.len());
    for export in &module.exports {
        text(&mut out, export.name.as_bytes());
        out.push(match export.kind {
            ExportKind::Function => 0,
            ExportKind::Memory => 2,
            ExportKind::Global => 3,
        });
        word(&mut out, export.index);
    }
    length(&mut out, module.data.len());
    for data in &module.data {
        word(&mut out, data.offset);
        text(&mut out, &data.bytes);
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}
