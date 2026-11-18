use std::fs;

#[path = "../src/emit/wasm/encode.rs"]
mod encode;
#[path = "../src/emit/wasm/flat.rs"]
mod flat;
#[path = "../src/emit/wasm/shake.rs"]
mod shake;

mod assemble {
    include!("../src/emit/wasm/assemble.rs");

    const CATALOG_SOURCE_BYTES_MAX: usize = 4194304;
    const CATALOG_ITEMS_MAX: usize = 65536;
    const CATALOG_TOKEN_HEADER_BYTES: usize = 9;

    fn catalog_word(out: &mut Vec<u8>, value: usize) {
        out.extend_from_slice(
            &u32::try_from(value)
                .expect("bounded catalog word")
                .to_le_bytes(),
        );
    }

    fn catalog_blob(out: &mut Vec<u8>, value: &[u8]) {
        catalog_word(out, value.len());
        out.extend_from_slice(value);
    }

    fn catalog_size(form: &Sexp) -> usize {
        let mut pending = vec![(form, 0)];
        let mut bytes = 0;
        for _ in 0..CATALOG_SOURCE_BYTES_MAX {
            let Some((node, depth)) = pending.pop() else {
                return bytes;
            };
            assert!(depth <= NESTING_MAX);
            match node {
                Sexp::List(items, _) => {
                    bytes += 2 * CATALOG_TOKEN_HEADER_BYTES;
                    pending.extend(items.iter().rev().map(|child| (child, depth + 1)));
                }
                Sexp::Atom(value, _) => bytes += CATALOG_TOKEN_HEADER_BYTES + value.len(),
                Sexp::Text(value, _) => bytes += CATALOG_TOKEN_HEADER_BYTES + value.len(),
            }
        }
        panic!("bounded AST traversal exhausted")
    }

    fn catalog_codes(out: &mut Vec<u8>, values: &[ValType]) {
        let bytes: Vec<u8> = values.iter().map(|value| value.code()).collect();
        catalog_blob(out, &bytes);
    }

    fn catalog_record(parsed: &Header, start: usize, end: usize, index: u32, ty: u32) -> Vec<u8> {
        let mut out = Vec::new();
        catalog_blob(
            &mut out,
            parsed.name.as_ref().expect("named header").as_bytes(),
        );
        catalog_word(&mut out, index as usize);
        catalog_word(&mut out, ty as usize);
        catalog_word(&mut out, start);
        catalog_word(&mut out, end);
        catalog_codes(
            &mut out,
            &parsed.params.iter().map(|(_, ty)| *ty).collect::<Vec<_>>(),
        );
        catalog_codes(
            &mut out,
            &parsed.locals.iter().map(|(_, ty)| *ty).collect::<Vec<_>>(),
        );
        let named: Vec<_> = parsed
            .params
            .iter()
            .chain(&parsed.locals)
            .enumerate()
            .filter_map(|(index, (name, _))| name.as_ref().map(|name| (name, index)))
            .collect();
        catalog_word(&mut out, named.len());
        for (name, index) in named {
            catalog_blob(&mut out, name.as_bytes());
            catalog_word(&mut out, index);
        }
        catalog_word(&mut out, usize::from(parsed.export.is_some()));
        catalog_blob(&mut out, parsed.export.as_deref().unwrap_or("").as_bytes());
        out
    }

    fn catalog_ascii_fields(parsed: &Header) -> Result<(), String> {
        if parsed.export.as_ref().is_some_and(|name| !name.is_ascii()) {
            return Err("catalog unsupported: nonascii export".to_owned());
        }
        if parsed.params.len() + parsed.locals.len() > CATALOG_ITEMS_MAX
            || parsed.results.len() > CATALOG_ITEMS_MAX
        {
            return Err("catalog bound: locals_max".to_owned());
        }
        Ok(())
    }

    fn catalog_import_records(
        forms: &[Sexp],
        module: &mut Module,
        names: &mut Names,
        catalog_names: &mut Vec<u8>,
    ) -> Result<(), String> {
        for form in forms {
            if form.head() == Some("import") {
                let Sexp::List(items, _) = form else {
                    unreachable!()
                };
                let Some(Sexp::List(signature, line)) = items.get(3) else {
                    return Err("import signature".to_owned());
                };
                if signature.first().and_then(Sexp::atom) != Some("func") {
                    return Err("import func".to_owned());
                }
                let parsed = header(signature, *line)?;
                catalog_ascii_fields(&parsed)?;
                for field in items.iter().skip(1).take(2) {
                    let Sexp::Text(value, _) = field else {
                        return Err("import text".to_owned());
                    };
                    if !value.is_ascii() {
                        return Err("catalog unsupported: nonascii import".to_owned());
                    }
                }
                let index = module.imports.len();
                import(form, module, names)?;
                catalog_blob(
                    catalog_names,
                    parsed.name.as_ref().expect("named header").as_bytes(),
                );
                catalog_word(catalog_names, index);
            }
        }
        Ok(())
    }

    fn catalog_function_records(
        forms: &[Sexp],
        module: &mut Module,
        names: &mut Names,
        catalog_names: &mut Vec<u8>,
    ) -> Result<(Vec<u8>, usize), String> {
        let mut functions = Vec::new();
        let mut count = 0;
        let mut offset = 0;
        for form in forms {
            let form_size = catalog_size(form);
            match form.head() {
                Some("func") => {
                    let Sexp::List(items, line) = form else {
                        unreachable!()
                    };
                    let parsed = header(items, *line)?;
                    catalog_ascii_fields(&parsed)?;
                    let index = index_u32(module.imports.len() + count);
                    let name = parsed.name.as_ref().expect("named header");
                    if names.functions.insert(name.clone(), index).is_some() {
                        return Err("function named twice".to_owned());
                    }
                    let ty = module.type_index(FuncType {
                        params: parsed.params.iter().map(|(_, ty)| *ty).collect(),
                        results: parsed.results.clone(),
                    });
                    let body_start = offset
                        + CATALOG_TOKEN_HEADER_BYTES
                        + items[..parsed.body_at]
                            .iter()
                            .map(catalog_size)
                            .sum::<usize>();
                    functions.extend(catalog_record(
                        &parsed,
                        body_start,
                        offset + form_size - CATALOG_TOKEN_HEADER_BYTES,
                        index,
                        ty,
                    ));
                    catalog_blob(catalog_names, name.as_bytes());
                    catalog_word(catalog_names, index as usize);
                    count += 1;
                }
                Some("import" | "memory" | "global" | "data" | "text" | "words") => {}
                _ => return Err("catalog unsupported: top level".to_owned()),
            }
            offset += form_size;
        }
        Ok((functions, count))
    }

    fn catalog_serialize(
        module: &Module,
        functions: Vec<u8>,
        count: usize,
        catalog_names: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        let mut types = Vec::new();
        catalog_word(&mut types, module.types.len());
        for ty in &module.types {
            catalog_codes(&mut types, &ty.params);
            catalog_codes(&mut types, &ty.results);
        }
        let mut imports = Vec::new();
        catalog_word(&mut imports, module.imports.len());
        for entry in &module.imports {
            catalog_blob(&mut imports, entry.module.as_bytes());
            catalog_blob(&mut imports, entry.name.as_bytes());
            catalog_word(&mut imports, entry.ty as usize);
        }
        let mut function_records = Vec::new();
        catalog_word(&mut function_records, count);
        function_records.extend(functions);
        let mut name_records = Vec::new();
        catalog_word(&mut name_records, module.imports.len() + count);
        name_records.extend(catalog_names);
        let mut out = Vec::new();
        catalog_word(&mut out, 1);
        for section in [types, imports, function_records, name_records] {
            catalog_blob(&mut out, &section);
        }
        if out.len() > CATALOG_SOURCE_BYTES_MAX {
            return Err("catalog bound: catalog_bytes_max".to_owned());
        }
        Ok(out)
    }

    pub fn catalog_projection(source: &str) -> Result<Vec<u8>, String> {
        if source.len() > CATALOG_SOURCE_BYTES_MAX {
            return Err("bound(source_bytes_max)".to_owned());
        }
        let forms = tokenize(source)?;
        if forms.len() > CATALOG_ITEMS_MAX {
            return Err("catalog bound: items_max forms".to_owned());
        }
        let mut module = Module::default();
        let mut names = Names {
            functions: BTreeMap::new(),
            globals: BTreeMap::new(),
            symbols: BTreeMap::new(),
        };
        let mut catalog_names = Vec::new();
        catalog_import_records(&forms, &mut module, &mut names, &mut catalog_names)?;
        let (functions, count) =
            catalog_function_records(&forms, &mut module, &mut names, &mut catalog_names)?;
        catalog_serialize(&module, functions, count, catalog_names)
    }
}

fn main() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args().collect();
    let [_, source_path, output_path] = arguments.as_slice() else {
        return Err("usage: selfhost-catalog-oracle source.wat output.bin".to_owned());
    };
    let source = fs::read_to_string(source_path).map_err(|error| error.to_string())?;
    match assemble::catalog_projection(&source) {
        Ok(bytes) => fs::write(output_path, bytes).map_err(|error| error.to_string()),
        Err(error) => Err(error),
    }
}
