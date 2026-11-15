use super::decode::{Module, Reader};

pub const INLINE_BYTES_MAX: usize = 64;
const BLOCK_EMPTY: u8 = 0x40;
const BLOCK_I32: u8 = 0x7f;
const BLOCK_I64: u8 = 0x7e;
const FLOAT_ACCESSES: [u8; 4] = [0x2a, 0x2b, 0x38, 0x39];

struct Leaf {
    bytes: Vec<u8>,
    calls: Vec<usize>,
}

pub fn leaves(module: &Module, bodies: &[(usize, &[u8])]) -> Vec<Option<Vec<u8>>> {
    let scanned: Vec<Option<Leaf>> = bodies
        .iter()
        .enumerate()
        .map(|(index, (base, body))| scan(module, index, *base, body))
        .collect();
    let mut sizes: Vec<Option<usize>> = vec![None; scanned.len()];
    for _ in 0..=scanned.len() {
        let mut changed = false;
        for (index, leaf) in scanned.iter().enumerate() {
            let Some(leaf) = leaf else { continue };
            if sizes[index].is_some() {
                continue;
            }
            let mut total = Some(leaf.bytes.len());
            for call in &leaf.calls {
                total = total.zip(sizes[*call]).map(|(total, size)| total + size);
            }
            if let Some(total) = total.filter(|total| *total <= INLINE_BYTES_MAX) {
                sizes[index] = Some(total);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    scanned
        .into_iter()
        .zip(sizes)
        .map(|(leaf, size)| size.and(leaf.map(|leaf| leaf.bytes)))
        .collect()
}

fn scan(module: &Module, index: usize, base: usize, body: &[u8]) -> Option<Leaf> {
    let mut reader = Reader::new(body, base);
    let params = module.types[module.function_types[index] as usize]
        .params
        .len();
    let groups = reader.count("local groups").ok()?;
    for _ in 0..groups {
        if reader.u32().ok()? != 0 {
            return None;
        }
        reader.value_type().ok()?;
    }
    let start = reader.offset() - base;
    let mut calls = Vec::new();
    let mut depth: usize = 0;
    loop {
        let opcode = reader.byte().ok()?;
        match opcode {
            0x0b if depth == 0 => {
                if !reader.done() {
                    return None;
                }
                let end = reader.offset() - base - 1;
                return Some(Leaf {
                    bytes: body[start..end].to_vec(),
                    calls,
                });
            }
            0x0b => depth -= 1,
            0x02 | 0x04 => {
                if ![BLOCK_EMPTY, BLOCK_I32, BLOCK_I64].contains(&reader.byte().ok()?) {
                    return None;
                }
                depth += 1;
            }
            0x05 if depth > 0 => {}
            0x1a | 0x1b | 0x45..=0x8a | 0xa7 | 0xac | 0xad => {}
            0x20 => {
                if reader.u32().ok()? as usize >= params {
                    return None;
                }
            }
            0x23 | 0x24 => {
                reader.u32().ok()?;
            }
            0x28..=0x3e if !FLOAT_ACCESSES.contains(&opcode) => {
                reader.u32().ok()?;
                reader.u32().ok()?;
            }
            0x3f | 0x40 => {
                reader.byte().ok()?;
            }
            0x41 => {
                reader.s32().ok()?;
            }
            0x42 => {
                reader.s64().ok()?;
            }
            0x10 => {
                let function = reader.u32().ok()? as usize;
                let local = function.checked_sub(module.imports.len())?;
                if local >= module.function_types.len() {
                    return None;
                }
                calls.push(local);
            }
            _ => return None,
        }
    }
}
