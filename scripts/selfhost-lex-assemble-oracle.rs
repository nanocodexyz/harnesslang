use std::fs;

#[path = "../src/emit/wasm/assemble.rs"]
mod assemble;
#[path = "../src/emit/wasm/encode.rs"]
mod encode;
#[path = "../src/emit/wasm/flat.rs"]
mod flat;
#[path = "../src/emit/wasm/shake.rs"]
mod shake;

const SOURCE_BYTES_MAX: usize = 4194304;

fn main() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().collect();
    let [_, input, flat_path, wasm_path] = arguments.as_slice() else {
        return Err(
            "usage: selfhost-lex-assemble-oracle <source.wat> <module.flat> <module.wasm>"
                .to_owned(),
        );
    };
    let source = fs::read_to_string(input).map_err(|error| error.to_string())?;
    if source.len() > SOURCE_BYTES_MAX {
        return Err("bound(source_bytes_max)".to_owned());
    }
    let module = assemble::assemble(&source)?;
    fs::write(flat_path, flat::flatten(&module)).map_err(|error| error.to_string())?;
    fs::write(wasm_path, module.encode()).map_err(|error| error.to_string())?;
    Ok(())
}
