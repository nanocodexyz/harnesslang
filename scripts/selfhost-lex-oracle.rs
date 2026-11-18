use std::fs;
use std::io::{self, Write};

const NESTING_MAX: usize = 4096;
const HEX_RADIX: u32 = 16;
const SOURCE_BYTES_MAX: usize = 4194304;
const TOKEN_BYTES_PER_SOURCE_BYTE: usize = 10;
const TOKEN_OPEN: u8 = 1;
const TOKEN_CLOSE: u8 = 2;
const TOKEN_ATOM: u8 = 3;
const TOKEN_TEXT: u8 = 4;

include!("../output/selfhost-lex/frozen.rs");

fn header(out: &mut Vec<u8>, kind: u8, line: usize, bytes: &[u8]) {
    out.push(kind);
    out.extend_from_slice(
        &u32::try_from(line)
            .expect("bounded source line")
            .to_le_bytes(),
    );
    out.extend_from_slice(
        &u32::try_from(bytes.len())
            .expect("bounded token")
            .to_le_bytes(),
    );
    out.extend_from_slice(bytes);
}

fn canonical(nodes: &[Sexp], source_bytes: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stack = vec![(nodes, 0, None)];
    let events_max = source_bytes + 1;
    for _ in 0..events_max {
        let Some((nodes, at, opened)) = stack.last_mut() else {
            break;
        };
        if *at == nodes.len() {
            if let Some(line) = *opened {
                header(&mut out, TOKEN_CLOSE, line, &[]);
            }
            stack.pop();
        } else {
            let current = *nodes;
            let node = &current[*at];
            *at += 1;
            match node {
                Sexp::List(items, line) => {
                    header(&mut out, TOKEN_OPEN, *line, &[]);
                    assert!(stack.len() <= NESTING_MAX);
                    stack.push((items, 0, Some(*line)));
                }
                Sexp::Atom(word, line) => header(&mut out, TOKEN_ATOM, *line, word.as_bytes()),
                Sexp::Text(bytes, line) => header(&mut out, TOKEN_TEXT, *line, bytes),
            }
        }
    }
    assert!(stack.is_empty());
    assert!(out.len() <= source_bytes * TOKEN_BYTES_PER_SOURCE_BYTE);
    out
}

fn main() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().collect();
    let [_, path] = arguments.as_slice() else {
        return Err("usage: selfhost-lex-oracle <source.wat>".to_owned());
    };
    let source = fs::read_to_string(path).map_err(|error| error.to_string())?;
    if source.len() > SOURCE_BYTES_MAX {
        return Err("bound(source_bytes_max)".to_owned());
    }
    let result = match tokenize(&source) {
        Ok(nodes) => {
            let bytes = canonical(&nodes, source.len());
            let mut result = String::from("ok ");
            use std::fmt::Write;
            for byte in bytes {
                write!(result, "{byte:02x}").expect("a string accepts formatted bytes");
            }
            result
        }
        Err(error) => error,
    };
    io::stdout()
        .write_all(result.as_bytes())
        .map_err(|error| error.to_string())
}
