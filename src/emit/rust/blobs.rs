use std::fmt::Write;

use crate::ir::{self, Program, Type};

use super::types::{constant, ident, sum_name, variant_ident};

pub fn emit(out: &mut String, program: &Program) {
    writeln!(
        out,
        "pub const HOLDS_BLOBS: bool = {};\n",
        program.blobs.is_some()
    )
    .expect("writing to a string succeeds");
    bounds(out, program);
    searches(out, program);
    state_blobs(out, program);
    if let Some(blobs) = &program.blobs {
        keep_blobs(out, program, blobs);
        dispatched(out, program, blobs);
    }
}

fn bound_of(tool: &ir::Tool) -> Option<&ir::Bound> {
    match &tool.returns {
        Type::Blob(bytes) => Some(bytes),
        _ => tool.attaches.as_ref(),
    }
}

fn bounds(out: &mut String, program: &Program) {
    let mut bound_arms = String::new();
    let mut returns_arms = String::new();
    for tool in &program.tools {
        let Some(bytes) = bound_of(tool) else {
            continue;
        };
        writeln!(
            bound_arms,
            "        {:?} => Some(({:?}, {})),",
            tool.name,
            bytes.name,
            constant(&bytes.name)
        )
        .expect("writing to a string succeeds");
        if matches!(tool.returns, Type::Blob(_)) && program.blobs.is_some() {
            writeln!(
                returns_arms,
                "        {:?} => Returns::Blob {{ name: {:?}, bytes: {} }},",
                tool.name,
                bytes.name,
                constant(&bytes.name)
            )
            .expect("writing to a string succeeds");
        }
    }
    writeln!(
        out,
        "pub fn blob_bound(tool: &str) -> Option<(&'static str, u64)> {{\n    match tool {{\n{bound_arms}        _ => None,\n    }}\n}}\n\npub fn dispatched_returns(tool: &str) -> Returns {{\n    match tool {{\n{returns_arms}        _ => Returns::Text,\n    }}\n}}\n"
    )
    .expect("writing to a string succeeds");
}

fn searches(out: &mut String, program: &Program) {
    let mut arms = String::new();
    for tool in &program.tools {
        if let Some(listing) = tool.finds {
            writeln!(arms, "        {:?} => Some({listing}),", tool.name)
                .expect("writing to a string succeeds");
        }
    }
    let deferred: Vec<String> = (0..program.listings.len())
        .map(|listing| program.deferred(listing).to_string())
        .collect();
    writeln!(
        out,
        "pub fn search_of(tool: &str) -> Option<usize> {{\n    match tool {{\n{arms}        _ => None,\n    }}\n}}\n\npub const DEFERRED: &[bool] = &[{}];\n",
        deferred.join(", ")
    )
    .expect("writing to a string succeeds");
}

fn walk(program: &Program, ty: &Type, value: &str, depth: usize) -> Option<String> {
    match ty {
        Type::Blob(_) => Some(format!("out.push({value}.digest.clone());")),
        Type::List { element, .. } => {
            let item = format!("item{depth}");
            let inner = walk(program, element, &item, depth + 1)?;
            Some(format!("for {item} in {value}.iter() {{ {inner} }}"))
        }
        Type::Known(inner) | Type::Maybe(inner) => {
            let prefix = if matches!(ty, Type::Known(_)) {
                "Known"
            } else {
                "Maybe"
            };
            let item = format!("item{depth}");
            let body = walk(program, inner, &item, depth + 1)?;
            Some(format!(
                "if let {prefix}::Ok({item}) = &{value} {{ {body} }}"
            ))
        }
        Type::Sum(sum) if ty.holds_blob(program) => {
            let mut arms = String::new();
            for variant in &program.sums[*sum].variants {
                let bodies: Vec<Option<String>> = variant
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(field, field_ty)| {
                        walk(program, field_ty, &format!("f{depth}_{field}"), depth + 1)
                    })
                    .collect();
                if bodies.iter().all(Option::is_none) {
                    continue;
                }
                let bindings: Vec<String> = (0..variant.fields.len())
                    .map(|field| format!("f{depth}_{field}"))
                    .collect();
                let body: Vec<String> = bodies.into_iter().flatten().collect();
                write!(
                    arms,
                    "{}::{}({}) => {{ {} }} ",
                    sum_name(program, *sum),
                    variant_ident(&variant.name),
                    bindings.join(", "),
                    body.join(" ")
                )
                .expect("writing to a string succeeds");
            }
            Some(format!("match &{value} {{ {arms}_ => {{}} }}"))
        }
        Type::U64 | Type::Bool | Type::Text | Type::Sum(_) => None,
    }
}

fn state_blobs(out: &mut String, program: &Program) {
    let mut body = String::new();
    for field in &program.state {
        let value = format!("state.{}", ident(&field.name));
        if let Some(walked) = walk(program, &field.ty, &value, 0) {
            writeln!(body, "    {walked}").expect("writing to a string succeeds");
        }
    }
    writeln!(
        out,
        "pub fn state_blobs(state: &State, out: &mut Vec<String>) {{\n{body}}}\n"
    )
    .expect("writing to a string succeeds");
}

fn keep_blobs(out: &mut String, program: &Program, blobs: &ir::Blobs) {
    let entry = sum_name(program, ir::ENTRY);
    let variants = &program.sums[ir::ENTRY].variants;
    let attached = format!("{entry}::{}", variant_ident(&variants[blobs.attached].name));
    let result = format!(
        "{entry}::{}",
        variant_ident(&variants[ir::ENTRY_RESULT].name)
    );
    writeln!(
        out,
        "pub fn keep_blobs(items: &mut [{entry}], kept: u64) {{\n    let total = items.iter().filter(|item| matches!(item, {attached}(..))).count() as u64;\n    let mut dropped = blobs_evicted(total, kept);\n    for item in items.iter_mut() {{\n        if dropped == 0 {{\n            break;\n        }}\n        let {attached}(call, blob) = item else {{\n            continue;\n        }};\n        *item = {result}(call.clone(), blobs_omitted(kept, &blob.digest));\n        dropped -= 1;\n    }}\n    let left = items.iter().filter(|item| matches!(item, {attached}(..))).count() as u64;\n    assert!(left <= kept, \"keep blobs leaves at most its count of blobs\");\n}}\n"
    )
    .expect("writing to a string succeeds");
}

fn dispatched(out: &mut String, program: &Program, blobs: &ir::Blobs) {
    let sum = sum_name(program, blobs.dispatched);
    let variant = |id: ir::VariantId| {
        format!(
            "{sum}::{}",
            variant_ident(&program.sums[blobs.dispatched].variants[id].name)
        )
    };
    writeln!(
        out,
        "pub fn dispatched(engine: &Engine<'_>, answer: Answer, blob_capable: bool) -> Result<{sum}, Halt> {{\n    match answer {{\n        Answer::Output(Output::Failed(error)) => Ok({failed}(error)),\n        Answer::Output(Output::Ok(json @ Json::Object(_))) if blob_capable => Ok({blob}(parse_blob(&json).map_err(|error| engine.corrupt(error))?)),\n        Answer::Output(Output::Ok(Json::String(text))) => Ok({ok}(text)),\n        Answer::Output(Output::Ok(json)) => Err(engine.corrupt(format!(\"{{}} is not text\", json.encode()))),\n        Answer::Unknown => Ok({unknown}),\n        Answer::Reply(_) => panic!(\"a dispatch gets no model reply\"),\n    }}\n}}\n",
        failed = variant(ir::FAILED),
        blob = variant(ir::DISPATCHED_BLOB),
        ok = variant(ir::OK),
        unknown = variant(ir::UNKNOWN),
    )
    .expect("writing to a string succeeds");
}
