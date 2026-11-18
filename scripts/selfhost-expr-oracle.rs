use std::fs;

#[path = "../src/emit/wasm/encode.rs"]
mod encode;
#[path = "../src/emit/wasm/flat.rs"]
mod flat;
#[path = "../src/emit/wasm/shake.rs"]
mod shake;

mod assemble {
    include!("../src/emit/wasm/assemble.rs");

    const EXPR_SOURCE_BYTES_MAX: usize = 4194304;

    pub fn expr_projection(source: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
        if source.len() > EXPR_SOURCE_BYTES_MAX {
            return Err("bound(source_bytes_max)".to_owned());
        }
        let forms = tokenize(source)?;
        let names = Names {
            functions: BTreeMap::new(),
            globals: BTreeMap::new(),
            symbols: BTreeMap::new(),
        };
        for form in &forms {
            if form.head() != Some("func") {
                continue;
            }
            let Sexp::List(items, line) = form else {
                unreachable!()
            };
            let parsed = header(items, *line)?;
            if parsed.name.as_deref() != Some("$f") {
                continue;
            }
            let mut module = Module::default();
            let ty = module.type_index(FuncType {
                params: parsed.params.iter().map(|(_, ty)| *ty).collect(),
                results: parsed.results.clone(),
            });
            let compiled = function(&names, &parsed, items, ty)?;
            let mut body = compiled.body.clone();
            assert_eq!(body.pop(), Some(OP_END));
            module.functions.push(compiled);
            module.exports.push(Export {
                name: "f".to_owned(),
                kind: ExportKind::Function,
                index: 0,
            });
            return Ok((body, module.encode()));
        }
        Err("selected function missing".to_owned())
    }
}

fn main() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().collect();
    let [_, input, body_path, wasm_path] = arguments.as_slice() else {
        return Err("usage: selfhost-expr-oracle SOURCE BODY WASM".to_owned());
    };
    let source = fs::read_to_string(input).map_err(|error| error.to_string())?;
    let (body, wasm) = assemble::expr_projection(&source)?;
    fs::write(body_path, body).map_err(|error| error.to_string())?;
    fs::write(wasm_path, wasm).map_err(|error| error.to_string())?;
    Ok(())
}
