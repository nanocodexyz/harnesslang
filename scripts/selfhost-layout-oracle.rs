use std::fs;

#[path = "../src/emit/wasm/encode.rs"]
mod encode;
#[path = "../src/emit/wasm/flat.rs"]
mod flat;
#[path = "../src/emit/wasm/shake.rs"]
mod shake;

mod assemble {
    include!("../src/emit/wasm/assemble.rs");

    pub fn static_projection(source: &str) -> Result<super::encode::Module, String> {
        let forms = tokenize(source)?;
        for form in &forms {
            if !matches!(form.head(), Some("text" | "words" | "global")) {
                return Err("projection requires only text, words and global forms".to_owned());
            }
        }
        let mut module = Module::default();
        let mut names = Names {
            functions: BTreeMap::new(),
            globals: BTreeMap::new(),
            symbols: BTreeMap::new(),
        };
        layout(&forms, &mut module, &mut names)?;
        for form in &forms {
            if let Sexp::List(items, line) = form
                && form.head() == Some("global")
            {
                global(items, *line, &mut module, &mut names)?;
            }
        }
        Ok(module)
    }
}

const SOURCE_BYTES_MAX: usize = 4194304;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, flat_path, wasm_path] = args.as_slice() else {
        return Err(
            "usage: selfhost-layout-oracle source.wat component.flat component.wasm".to_owned(),
        );
    };
    let source = fs::read_to_string(input).map_err(|error| error.to_string())?;
    if source.len() > SOURCE_BYTES_MAX {
        return Err("bound(source_bytes_max)".to_owned());
    }
    let module = assemble::static_projection(&source)?;
    fs::write(flat_path, flat::flatten(&module)).map_err(|error| error.to_string())?;
    fs::write(wasm_path, module.encode()).map_err(|error| error.to_string())?;
    Ok(())
}
