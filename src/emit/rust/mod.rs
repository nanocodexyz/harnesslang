mod blobs;
mod handlers;
mod sql;
mod types;

use std::fmt::Write;

use crate::effect::EffectClass;
use crate::ir::{self, Program};
use crate::target::Target;

use handlers::Body;
use types::{Types, camel, constant, ident, sum_name, variant_ident};

const RUNTIME_JOURNAL: &str = include_str!("runtime/journal.rs");
const RUNTIME_ENGINE: &str = include_str!("runtime/engine.rs");
const RUNTIME_BLOB: &str = include_str!("runtime/blob.rs");
const RUNTIME_CELL: &str = include_str!("runtime/cell.rs");
const RUNTIME_JSONL: &str = include_str!("runtime/jsonl.rs");
const SHARED_CELL_JOURNAL: &str = include_str!("../../run/cell_journal.rs");
const SHARED_IMPORTS: [&str; 3] = ["use std::", "use crate::run::", "use crate::effect::"];
const SHARED_PATHS: [&str; 1] = ["crate::run::journal::"];
const SHARED_STRUCT_RENAMES: [(&str, &str); 1] = [("Fields", "CellFields")];
const LINTS: &str = "#![allow(dead_code, unused_variables, unused_parens, unreachable_code, unused_comparisons, non_camel_case_types, clippy::all)]\n";

pub const TARGETS: [Target; 3] = [
    Target::NativeJsonl,
    Target::NativeSqlite,
    Target::ServerPostgres,
];

pub fn emit(program: &Program, target: Target) -> Result<String, String> {
    if !TARGETS.contains(&target) {
        return Err(format!(
            "the Rust backend emits {}, not {}",
            TARGETS.map(Target::name).join(", "),
            target.name()
        ));
    }
    check_names(program)?;
    let mut out = String::from(LINTS);
    out.push_str(RUNTIME_JOURNAL);
    out.push_str(RUNTIME_ENGINE);
    out.push_str(RUNTIME_BLOB);
    out.push_str(&shared_source(SHARED_CELL_JOURNAL));
    out.push_str(RUNTIME_CELL);
    match target {
        Target::NativeJsonl => out.push_str(RUNTIME_JSONL),
        Target::NativeSqlite | Target::ServerPostgres => out.push_str(&sql::store(target)?),
        Target::Cloudflare | Target::Vercel => unreachable!("refused above"),
    }
    out.push_str("\npub mod program {\n    use super::*;\n\n");
    out.push_str(&generate(program, target));
    out.push_str("}\n");
    Ok(out)
}

fn shared_source(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        if line.starts_with("use ") {
            assert!(
                SHARED_IMPORTS.iter().any(|prefix| line.starts_with(prefix)),
                "a shared runtime source imports only std and what the runtime defines, found {line}"
            );
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    for path in SHARED_PATHS {
        out = out.replace(path, "");
    }
    for (from, to) in SHARED_STRUCT_RENAMES {
        out = rename_word(&out, from, to);
    }
    assert!(
        !out.contains("crate::"),
        "a shared runtime source names nothing of the compiler"
    );
    out
}

fn rename_word(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find(from) {
        let before = rest[..index].chars().next_back();
        let after = rest[index + from.len()..].chars().next();
        let word =
            |character: Option<char>| character.is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(&rest[..index]);
        out.push_str(if word(before) || word(after) {
            from
        } else {
            to
        });
        rest = &rest[index + from.len()..];
    }
    out.push_str(rest);
    out
}

fn check_names(program: &Program) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for sum in &program.sums {
        let mut variants = std::collections::BTreeSet::new();
        for variant in &sum.variants {
            if !variants.insert(variant_ident(&variant.name)) {
                return Err(format!(
                    "type '{}': variants map to the same Rust name {}",
                    sum.name,
                    variant_ident(&variant.name)
                ));
            }
        }
        if !seen.insert(camel(&sum.name)) {
            return Err(format!(
                "type '{}' maps to a Rust name already taken",
                sum.name
            ));
        }
    }
    let mut constants = std::collections::BTreeSet::new();
    for item in &program.constants {
        if !constants.insert(constant(&item.name)) {
            return Err(format!(
                "constant '{}' maps to a Rust name already taken",
                item.name
            ));
        }
    }
    let mut handlers = std::collections::BTreeSet::new();
    for handler in &program.handlers {
        if !handlers.insert(camel(&handler.name)) {
            return Err(format!(
                "handler '{}' maps to a Rust name already taken",
                handler.name
            ));
        }
    }
    Ok(())
}

fn generate(program: &Program, target: Target) -> String {
    let mut types = Types::new(program);
    let mut out = String::new();
    let line = |out: &mut String, text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    line(
        &mut out,
        format!("pub const HARNESS_NAME: &str = {:?};", program.name),
    );
    line(
        &mut out,
        format!("pub const TARGET: &str = {:?};", target.name()),
    );
    let concurrent = crate::target::model(target)
        .limits
        .concurrent_requests
        .map_or_else(
            || "None".to_owned(),
            |limit| format!("Some({})", limit.value),
        );
    line(
        &mut out,
        format!("pub const CONCURRENT_REQUESTS_MAX: Option<u64> = {concurrent};"),
    );
    if let Some(model) = &program.model {
        line(
            &mut out,
            format!("pub const MODEL_NAME: &str = {:?};", model.name),
        );
    }
    for item in &program.constants {
        line(
            &mut out,
            format!("pub const {}: u64 = {};", constant(&item.name), item.value),
        );
    }
    let events = &program.events;
    line(
        &mut out,
        format!("pub const TAKES_CANCEL: bool = {};", events.cancel),
    );
    let steers = events.steers.as_ref().map_or_else(
        || "None".to_owned(),
        |bound| format!("Some(({:?}, {}))", bound.name, constant(&bound.name)),
    );
    line(
        &mut out,
        format!("pub const STEER_EVENT: Option<(&str, u64)> = {steers};"),
    );
    line(
        &mut out,
        format!(
            "pub const EVENT_RECORDS_MAX: u64 = {};",
            events.records_max()
        ),
    );
    out.push('\n');
    for (id, sum) in program.sums.iter().enumerate() {
        sum_definition(&mut out, &types, id, sum);
    }
    state(&mut out, program, &mut types);
    idle(&mut out, program);
    arguments(&mut out, program, &mut types);
    dispatch(&mut out, program);
    blobs::emit(&mut out, program);
    codes(&mut out, program);
    inputs(&mut out, program, &mut types);
    for handler in &program.handlers {
        handler_function(&mut out, program, handler, &mut types);
    }
    types.emit_codecs(&mut out);
    out
}

fn sum_definition(out: &mut String, types: &Types<'_>, id: ir::SumId, sum: &ir::Sum) {
    let program_variants: Vec<String> = sum
        .variants
        .iter()
        .map(|variant| {
            if variant.fields.is_empty() {
                variant_ident(&variant.name)
            } else {
                let fields: Vec<String> = variant
                    .fields
                    .iter()
                    .map(|field| types.rust(field))
                    .collect();
                format!("{}({})", variant_ident(&variant.name), fields.join(", "))
            }
        })
        .collect();
    let (derives, first) = match sum.kind {
        ir::SumKind::Tagged => ("Clone, Debug, PartialEq, Eq", ""),
        ir::SumKind::Text => ("Clone, Debug, Default, PartialEq, Eq", "#[default]\n    "),
    };
    let body: String = program_variants
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            let marker = if index == 0 { first } else { "" };
            format!("    {marker}{variant},\n")
        })
        .collect();
    writeln!(
        out,
        "#[derive({derives})]\npub enum {} {{\n{body}}}\n",
        sum_name(types.program(), id)
    )
    .expect("writing to a string succeeds");
}

fn state(out: &mut String, program: &Program, types: &mut Types<'_>) {
    let mut fields = String::new();
    let mut to_json = Vec::new();
    let mut from_json = String::new();
    for field in &program.state {
        let name = ident(&field.name);
        writeln!(fields, "    pub {name}: {},", types.rust(&field.ty))
            .expect("writing to a string succeeds");
        to_json.push(format!(
            "({:?}, {}(&state.{name}))",
            field.name,
            types.to_json(&field.ty)
        ));
        writeln!(
            from_json,
            "        {name}: {}(map.get({:?}).ok_or_else(|| {:?}.to_owned())?)?,",
            types.from_json(&field.ty),
            field.name,
            format!("state lacks field '{}'", field.name)
        )
        .expect("writing to a string succeeds");
    }
    let count = program.state.len();
    writeln!(
        out,
        "#[derive(Clone, Debug, Default, PartialEq, Eq)]\npub struct State {{\n{fields}}}\n\npub fn state_json(state: &State) -> Json {{\n    Json::object(vec![{}])\n}}\n\npub fn state_from_json(json: &Json) -> Result<State, String> {{\n    let Json::Object(map) = json else {{\n        return Err(\"state is not an object\".to_owned());\n    }};\n    if map.len() != {count} {{\n        return Err(format!(\"state has {{}} fields, the program has {count}\", map.len()));\n    }}\n    Ok(State {{\n{from_json}    }})\n}}\n",
        to_json.join(", ")
    )
    .expect("writing to a string succeeds");
}

fn idle(out: &mut String, program: &Program) {
    let Some(idle) = &program.idle else {
        writeln!(
            out,
            "pub const IDLE_HANDLER: Option<&str> = None;\n\npub fn idle_armed(_state: &State) -> Option<u64> {{\n    None\n}}\n"
        )
        .expect("writing to a string succeeds");
        return;
    };
    let field = |id: ir::FieldId| ident(&program.state[id].name);
    writeln!(
        out,
        "pub const IDLE_HANDLER: Option<&str> = Some({:?});\n\npub fn idle_armed(state: &State) -> Option<u64> {{\n    let due = state.{};\n    (due != 0 && state.{} < {}).then_some(due)\n}}\n",
        idle.handler,
        field(idle.due),
        field(idle.runs),
        constant(&idle.bound.name)
    )
    .expect("writing to a string succeeds");
}

fn arguments(out: &mut String, program: &Program, types: &mut Types<'_>) {
    let mut variants = String::new();
    let mut names = String::new();
    let mut to_json = String::new();
    let mut decode = String::new();
    let mut run = String::new();
    for handler in &program.handlers {
        let variant = camel(&handler.name);
        let params: Vec<&ir::Field> = handler
            .params
            .iter()
            .map(|local| &handler.locals[*local])
            .collect();
        let bindings: Vec<String> = (0..params.len()).map(|index| format!("p{index}")).collect();
        let pattern = if params.is_empty() {
            format!("Arguments::{variant}")
        } else {
            format!("Arguments::{variant}({})", bindings.join(", "))
        };
        let rust: Vec<String> = params.iter().map(|param| types.rust(&param.ty)).collect();
        if params.is_empty() {
            writeln!(variants, "    {variant},").expect("writing to a string succeeds");
        } else {
            writeln!(variants, "    {variant}({}),", rust.join(", "))
                .expect("writing to a string succeeds");
        }
        writeln!(names, "        {pattern} => {:?},", handler.name)
            .expect("writing to a string succeeds");
        let entries: Vec<String> = params
            .iter()
            .zip(&bindings)
            .map(|(param, binding)| {
                format!(
                    "({:?}, {}({binding}))",
                    param.name,
                    types.to_json(&param.ty)
                )
            })
            .collect();
        writeln!(
            to_json,
            "        {pattern} => Json::object(vec![{}]),",
            entries.join(", ")
        )
        .expect("writing to a string succeeds");
        let parsed: Vec<String> = params
            .iter()
            .map(|param| {
                format!(
                    "{}(json.get({:?}).ok_or_else(|| ArgumentsError::Invalid({:?}.to_owned()))?).map_err(ArgumentsError::Invalid)?",
                    types.from_json(&param.ty),
                    param.name,
                    format!("arguments lack '{}'", param.name)
                )
            })
            .collect();
        let value = if params.is_empty() {
            format!("Arguments::{variant}")
        } else {
            format!("Arguments::{variant}({})", parsed.join(", "))
        };
        writeln!(decode, "        {:?} => Ok({value}),", handler.name)
            .expect("writing to a string succeeds");
        let clones: Vec<String> = bindings
            .iter()
            .map(|binding| format!("{binding}.clone()"))
            .collect();
        let call_args = if clones.is_empty() {
            String::new()
        } else {
            format!(", {}", clones.join(", "))
        };
        writeln!(
            run,
            "        {pattern} => Ok({}(&handler_{}(engine, state{call_args})?)),",
            types.to_json(&handler.returns),
            ident(&handler.name)
        )
        .expect("writing to a string succeeds");
    }
    writeln!(
        out,
        "#[derive(Clone, Debug, PartialEq, Eq)]\npub enum Arguments {{\n{variants}}}\n\n#[derive(Clone, Debug, PartialEq, Eq)]\npub enum ArgumentsError {{\n    NoHandler,\n    Invalid(String),\n}}\n\npub fn handler_name(arguments: &Arguments) -> &'static str {{\n    match arguments {{\n{names}    }}\n}}\n\npub fn arguments_json(arguments: &Arguments) -> Json {{\n    match arguments {{\n{to_json}    }}\n}}\n\npub fn decode_arguments(handler: &str, json: &Json) -> Result<Arguments, ArgumentsError> {{\n    match handler {{\n{decode}        _ => Err(ArgumentsError::NoHandler),\n    }}\n}}\n\npub fn run_body(engine: &mut Engine<'_>, state: &mut State, arguments: &Arguments) -> Result<Json, Halt> {{\n    match arguments {{\n{run}    }}\n}}\n"
    )
    .expect("writing to a string succeeds");
}

fn dispatch(out: &mut String, program: &Program) {
    let mut arms = String::new();
    for tool in &program.tools {
        writeln!(
            arms,
            "        {:?} => EffectClass::{:?},",
            tool.name, tool.effect
        )
        .expect("writing to a string succeeds");
    }
    let mut listed = String::new();
    let mut listings = Vec::new();
    for (id, listing) in program.listings.iter().enumerate() {
        writeln!(
            listed,
            "            Some(({id}, _)) => EffectClass::{:?},",
            listing.effect
        )
        .expect("writing to a string succeeds");
        listings.push(format!(
            "({:?}, {:?}, {})",
            listing.name,
            listing.bound.name,
            constant(&listing.bound.name)
        ));
    }
    writeln!(
        out,
        "pub const LISTINGS: &[(&str, &str, u64)] = &[{}];\n",
        listings.join(", ")
    )
    .expect("writing to a string succeeds");
    writeln!(
        out,
        "pub fn dispatch_class(tool: &str) -> EffectClass {{\n    match tool {{\n{arms}        _ => match listed(tool) {{\n{listed}            _ => EffectClass::{:?},\n        }},\n    }}\n}}\n",
        EffectClass::undeclared()
    )
    .expect("writing to a string succeeds");
    let spawns: Vec<String> = program
        .spawns()
        .map(|(tool, _)| format!("{:?}", tool.name))
        .collect();
    let pattern = if spawns.is_empty() {
        "_tool: &str) -> bool {\n    false".to_owned()
    } else {
        format!(
            "tool: &str) -> bool {{\n    matches!(tool, {})",
            spawns.join(" | ")
        )
    };
    writeln!(out, "pub fn spawns({pattern}\n}}\n").expect("writing to a string succeeds");
}

fn limit(bound: &ir::Bound) -> String {
    format!(
        "Limit {{ name: {:?}, value: {} }}",
        bound.name,
        constant(&bound.name)
    )
}

fn nested_tool(program: &Program, tool: ir::ToolId) -> String {
    let tool = &program.tools[tool];
    let input: Vec<String> = tool
        .params
        .iter()
        .map(|param| format!("({:?}, {:?})", param.name, param.ty.name(program)))
        .collect();
    format!(
        "NestedTool {{ name: {:?}, effect: EffectClass::{:?}, returns: {}, input: &[{}] }}",
        tool.name,
        tool.effect,
        Types::new(program).returns(&tool.returns),
        input.join(", ")
    )
}

fn codes(out: &mut String, program: &Program) {
    let mut specs = String::new();
    let mut arms = String::new();
    for (index, code) in program.codes.iter().enumerate() {
        let tools: Vec<String> = code
            .tools
            .iter()
            .map(|tool| nested_tool(program, *tool))
            .collect();
        writeln!(
            specs,
            "    CodeSpec {{ name: {:?}, tools: &[{}], calls: {}, parallel: {}, timers: {}, timer_ms: {}, journal_bytes: {}, yield_ms: {} }},",
            code.name,
            tools.join(", "),
            limit(&code.calls),
            limit(&code.parallel),
            limit(&code.timers),
            limit(&code.timer_ms),
            limit(&code.journal_bytes),
            limit(&code.yield_ms)
        )
        .expect("writing to a string succeeds");
        writeln!(arms, "        {:?} => Some({index}),", code.name)
            .expect("writing to a string succeeds");
    }
    writeln!(
        out,
        "pub const CODES: &[CodeSpec] = &[\n{specs}];\n\npub fn code_of(tool: &str) -> Option<usize> {{\n    match tool {{\n{arms}        _ => None,\n    }}\n}}\n"
    )
    .expect("writing to a string succeeds");
}

fn inputs<'p>(out: &mut String, program: &'p Program, types: &mut Types<'p>) {
    let mut arms = String::new();
    let mut listed = String::new();
    let declared = program
        .tools
        .iter()
        .map(|tool| (tool.name.as_str(), &tool.params, &tool.defaults));
    let families = program
        .listings
        .iter()
        .map(|listing| (listing.name.as_str(), &listing.params, &listing.defaults));
    for (index, (name, params, defaults)) in declared.chain(families).enumerate() {
        let names: Vec<String> = params
            .iter()
            .map(|param| format!("{:?}", param.name))
            .collect();
        let mut checks = String::new();
        for (param, default) in params.iter().zip(defaults) {
            if let Some(default) = default {
                let value = match default {
                    ir::Literal::U64(value) => format!("Json::Number({value})"),
                    ir::Literal::Bool(value) => format!("Json::Bool({value})"),
                    ir::Literal::Text(value) => format!("Json::String(String::from({value:?}))"),
                };
                writeln!(
                    checks,
                    "    input_default(input, {:?}, {value});",
                    param.name
                )
                .expect("writing to a string succeeds");
            }
        }
        checks.push_str(
            "    let fields = input_fields(input)?;
",
        );
        for param in params {
            writeln!(
                checks,
                "    input_param(fields, {:?}, {})?;",
                param.name,
                types.from_json(&param.ty)
            )
            .expect("writing to a string succeeds");
        }
        writeln!(
            out,
            "fn input_{index}(input: &mut Json) -> Result<(), String> {{\n{checks}    input_extra(fields, &[{}])\n}}\n",
            names.join(", ")
        )
        .expect("writing to a string succeeds");
        match index.checked_sub(program.tools.len()) {
            None => writeln!(arms, "        {name:?} => input_{index},"),
            Some(listing) => writeln!(listed, "            Some(({listing}, _)) => input_{index},"),
        }
        .expect("writing to a string succeeds");
    }
    writeln!(
        out,
        "pub fn tool_input(tool: &str, raw: &str) -> ToolInput {{\n    let check: fn(&mut Json) -> Result<(), String> = match tool {{\n{arms}        _ => match listed(tool) {{\n{listed}            _ => return ToolInput::Untyped,\n        }},\n    }};\n    typed_input(tool, raw, check)\n}}\n"
    )
    .expect("writing to a string succeeds");
}

fn handler_function<'p>(
    out: &mut String,
    program: &'p Program,
    handler: &'p ir::Handler,
    types: &mut Types<'p>,
) {
    let returns = types.rust(&handler.returns);
    let mut body = Body::new(program, handler, types);
    let params: Vec<String> = handler
        .params
        .iter()
        .map(|local| {
            format!(
                ", {}: {}",
                body.local(*local),
                body.types.rust(&handler.locals[*local].ty)
            )
        })
        .collect();
    body.emit();
    writeln!(
        out,
        "pub fn handler_{}(engine: &mut Engine<'_>, state: &mut State{}) -> Result<{returns}, Halt> {{\n{}}}\n",
        ident(&handler.name),
        params.concat(),
        body.out
    )
    .expect("writing to a string succeeds");
}
