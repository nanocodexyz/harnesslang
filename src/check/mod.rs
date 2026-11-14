mod handler;
mod inline;

pub use inline::STATEMENTS_PER_HANDLER_MAX;

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{self, Bound, Sum, Type, Variant};
use crate::syntax::ast::{self, Item, TypeExpr};
use crate::syntax::{Diagnostic, Position};

pub const BUILTIN_TYPES: [&str; 6] = ["u64", "bool", "text", "list", "known", "maybe"];
pub const BUILTIN_SUM_NAMES: [&str; 3] = ["call", "reply", "entry"];
pub const BUILTIN_FUNCTIONS: [&str; 4] = ["dispatch", "now", "random", "len"];
pub const WILDCARD: &str = "_";

pub struct Globals {
    constants: BTreeMap<String, ir::ConstId>,
    types: BTreeMap<String, ir::SumId>,
    tools: BTreeMap<String, ir::ToolId>,
    fields: BTreeMap<String, ir::FieldId>,
    model: Option<String>,
}

type Checked<T> = Result<T, Diagnostic>;

fn error<T>(at: Position, message: impl Into<String>) -> Checked<T> {
    Err(Diagnostic::new(at, message))
}

pub fn check(source: &ast::Program) -> Checked<ir::Program> {
    let mut program = ir::Program {
        name: source.name.text.clone(),
        constants: Vec::new(),
        sums: Vec::new(),
        tools: Vec::new(),
        model: None,
        state: Vec::new(),
        invariants: Vec::new(),
        handlers: Vec::new(),
    };
    let mut globals = Globals {
        constants: BTreeMap::new(),
        types: BTreeMap::new(),
        tools: BTreeMap::new(),
        fields: BTreeMap::new(),
        model: None,
    };
    let mut values: BTreeMap<String, Position> = BTreeMap::new();
    constants(source, &mut program, &mut globals, &mut values)?;
    model(source, &mut program, &mut globals, &mut values)?;
    sums(source, &mut program, &mut globals)?;
    tools(source, &mut program, &mut globals, &mut values)?;
    state(source, &mut program, &mut globals, &mut values)?;
    let expanded = inline::expand(source, &program, &globals, &mut values)?;
    invariants(&expanded, &mut program, &globals)?;
    handlers(&expanded, &mut program, &globals)?;
    ir::validate::program(&program);
    Ok(program)
}

fn claim(values: &mut BTreeMap<String, Position>, name: &ast::Name, what: &str) -> Checked<()> {
    if name.text.starts_with(WILDCARD) {
        return error(
            name.at,
            format!(
                "{what} '{}': names starting with '_' are reserved",
                name.text
            ),
        );
    }
    if BUILTIN_FUNCTIONS.contains(&name.text.as_str()) {
        return error(
            name.at,
            format!("{what} '{}' has the name of a builtin", name.text),
        );
    }
    if let Some(previous) = values.get(&name.text) {
        return error(
            name.at,
            format!("'{}' is already declared at {previous}", name.text),
        );
    }
    values.insert(name.text.clone(), name.at);
    Ok(())
}

fn constants(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    for item in &source.items {
        let Item::Const(constant) = item else {
            continue;
        };
        claim(values, &constant.name, "constant")?;
        match &constant.ty {
            TypeExpr::Named(name) if name.text == "u64" => {}
            other => return error(other.at(), "constants are u64"),
        }
        globals
            .constants
            .insert(constant.name.text.clone(), program.constants.len());
        program.constants.push(ir::Constant {
            name: constant.name.text.clone(),
            value: constant.value,
        });
    }
    Ok(())
}

fn bound(program: &ir::Program, globals: &Globals, name: &ast::Name, what: &str) -> Checked<Bound> {
    match globals.constants.get(&name.text) {
        Some(id) => Ok(Bound {
            name: name.text.clone(),
            value: program.constants[*id].value,
        }),
        None => error(
            name.at,
            format!(
                "{what} must be a named constant: '{}' is not a constant",
                name.text
            ),
        ),
    }
}

fn model(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    for item in &source.items {
        let Item::Model(model) = item else { continue };
        if let Some(previous) = &program.model {
            return error(
                model.name.at,
                format!(
                    "a harness has one model; '{}' is already declared",
                    previous.name
                ),
            );
        }
        claim(values, &model.name, "model")?;
        let calls = bound(program, globals, &model.calls, "the model's calls bound")?;
        globals.model = Some(model.name.text.clone());
        program.model = Some(ir::Model {
            name: model.name.text.clone(),
            calls,
        });
    }
    Ok(())
}

fn builtin_sums(program: &ir::Program) -> Vec<Sum> {
    let call = Sum {
        name: BUILTIN_SUM_NAMES[ir::CALL].to_owned(),
        variants: vec![Variant {
            name: BUILTIN_SUM_NAMES[ir::CALL].to_owned(),
            fields: vec![Type::Text, Type::Text, Type::Text],
        }],
    };
    let Some(model) = &program.model else {
        let placeholder = |name: &str| Sum {
            name: name.to_owned(),
            variants: Vec::new(),
        };
        return vec![
            call,
            placeholder(BUILTIN_SUM_NAMES[ir::REPLY]),
            placeholder(BUILTIN_SUM_NAMES[ir::ENTRY]),
        ];
    };
    let variant = |name: &str, fields: Vec<Type>| Variant {
        name: name.to_owned(),
        fields,
    };
    let calls = Type::List {
        element: Box::new(Type::Sum(ir::CALL)),
        capacity: model.calls.clone(),
    };
    let reply = Sum {
        name: BUILTIN_SUM_NAMES[ir::REPLY].to_owned(),
        variants: vec![
            variant("answer", vec![Type::Text]),
            variant("calls", vec![calls]),
        ],
    };
    let call_type = Type::Sum(ir::CALL);
    let entry = Sum {
        name: BUILTIN_SUM_NAMES[ir::ENTRY].to_owned(),
        variants: vec![
            variant("user", vec![Type::Text]),
            variant("assistant", vec![Type::Sum(ir::REPLY)]),
            variant("result", vec![call_type.clone(), Type::Text]),
            variant("error", vec![call_type.clone(), Type::Text]),
            variant("unknown", vec![call_type]),
        ],
    };
    vec![call, reply, entry]
}

fn sums(source: &ast::Program, program: &mut ir::Program, globals: &mut Globals) -> Checked<()> {
    program.sums = builtin_sums(program);
    for (id, name) in BUILTIN_SUM_NAMES.iter().enumerate() {
        globals.types.insert((*name).to_owned(), id);
    }
    let mut declared = Vec::new();
    for item in &source.items {
        let Item::Type(definition) = item else {
            continue;
        };
        let name = &definition.name;
        if BUILTIN_TYPES.contains(&name.text.as_str()) || globals.types.contains_key(&name.text) {
            return error(
                name.at,
                format!("type '{}' is already declared or builtin", name.text),
            );
        }
        globals.types.insert(name.text.clone(), program.sums.len());
        program.sums.push(Sum {
            name: name.text.clone(),
            variants: Vec::new(),
        });
        declared.push(definition);
    }
    for definition in declared {
        let id = globals.types[&definition.name.text];
        let mut seen = BTreeSet::new();
        let mut variants = Vec::new();
        for variant in &definition.variants {
            if !seen.insert(variant.name.text.clone()) {
                return error(
                    variant.name.at,
                    format!(
                        "variant '{}' appears twice in '{}'",
                        variant.name.text, definition.name.text
                    ),
                );
            }
            let mut fields = Vec::new();
            for field in &variant.fields {
                fields.push(resolve(program, globals, field)?);
            }
            variants.push(Variant {
                name: variant.name.text.clone(),
                fields,
            });
        }
        program.sums[id].variants = variants;
    }
    acyclic(source, program, globals)
}

fn contained(ty: &Type, out: &mut Vec<ir::SumId>) {
    match ty {
        Type::U64 | Type::Bool | Type::Text => {}
        Type::Sum(sum) => out.push(*sum),
        Type::List { element, .. } => contained(element, out),
        Type::Known(inner) | Type::Maybe(inner) => contained(inner, out),
    }
}

fn acyclic(source: &ast::Program, program: &ir::Program, globals: &Globals) -> Checked<()> {
    for item in &source.items {
        let Item::Type(definition) = item else {
            continue;
        };
        let start = globals.types[&definition.name.text];
        let mut stack = vec![start];
        let mut visited = BTreeSet::new();
        while let Some(sum) = stack.pop() {
            assert!(
                visited.len() <= program.sums.len(),
                "a walk visits each sum at most once"
            );
            let mut next = Vec::new();
            for variant in &program.sums[sum].variants {
                variant
                    .fields
                    .iter()
                    .for_each(|field| contained(field, &mut next));
            }
            for inner in next {
                if inner == start {
                    return error(
                        definition.name.at,
                        format!(
                            "type '{}' contains itself, so its values have no bound",
                            definition.name.text
                        ),
                    );
                }
                if visited.insert(inner) {
                    stack.push(inner);
                }
            }
        }
    }
    Ok(())
}

pub fn resolve(program: &ir::Program, globals: &Globals, ty: &TypeExpr) -> Checked<Type> {
    match ty {
        TypeExpr::Named(name) => match name.text.as_str() {
            "u64" => Ok(Type::U64),
            "bool" => Ok(Type::Bool),
            "text" => Ok(Type::Text),
            other => match globals.types.get(other) {
                Some(&sum) if (sum == ir::REPLY || sum == ir::ENTRY) && program.model.is_none() => {
                    error(
                        name.at,
                        format!(
                            "type '{other}' needs a model declaration: its replies are bounded by the model's calls"
                        ),
                    )
                }
                Some(&sum) => Ok(Type::Sum(sum)),
                None => error(name.at, format!("unknown type '{other}'")),
            },
        },
        TypeExpr::List {
            element, capacity, ..
        } => Ok(Type::List {
            element: Box::new(resolve(program, globals, element)?),
            capacity: bound(program, globals, capacity, "a list's capacity")?,
        }),
        TypeExpr::Known { inner, .. } => {
            Ok(Type::Known(Box::new(resolve(program, globals, inner)?)))
        }
        TypeExpr::Maybe { inner, .. } => {
            Ok(Type::Maybe(Box::new(resolve(program, globals, inner)?)))
        }
    }
}

fn variant_names(program: &ir::Program) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = ir::OUTCOME_VARIANTS
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    for sum in &program.sums {
        names.extend(sum.variants.iter().map(|variant| variant.name.clone()));
    }
    names
}

fn tools(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    let variants = variant_names(program);
    for item in &source.items {
        let Item::Tool(tool) = item else { continue };
        claim(values, &tool.name, "tool")?;
        if variants.contains(&tool.name.text) {
            return error(
                tool.name.at,
                format!("tool '{}' has the name of a variant", tool.name.text),
            );
        }
        let mut params = Vec::new();
        let mut seen = BTreeSet::new();
        for param in &tool.params {
            if !seen.insert(param.name.text.clone()) {
                return error(
                    param.name.at,
                    format!("parameter '{}' appears twice", param.name.text),
                );
            }
            params.push(ir::Field {
                name: param.name.text.clone(),
                ty: resolve(program, globals, &param.ty)?,
            });
        }
        let returns = resolve(program, globals, &tool.returns)?;
        if !matches!(returns, Type::U64 | Type::Bool | Type::Text) {
            return error(tool.returns.at(), "a tool returns u64, bool or text");
        }
        globals
            .tools
            .insert(tool.name.text.clone(), program.tools.len());
        program.tools.push(ir::Tool {
            name: tool.name.text.clone(),
            params,
            returns,
            effect: tool.effect,
        });
    }
    Ok(())
}

fn state(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    let mut seen_state = None;
    for item in &source.items {
        let Item::State(state) = item else { continue };
        if let Some(previous) = seen_state {
            return error(
                state.at,
                format!("a harness has one state block; one is already declared at {previous}"),
            );
        }
        seen_state = Some(state.at);
        for field in &state.fields {
            claim(values, &field.name, "state field")?;
            let ty = resolve(program, globals, &field.ty)?;
            if crate::run::value::Value::zero(&ty).is_none() {
                return error(
                    field.ty.at(),
                    format!(
                        "state field '{}' has no initial value: state fields are u64, bool, text or lists",
                        field.name.text
                    ),
                );
            }
            globals
                .fields
                .insert(field.name.text.clone(), program.state.len());
            program.state.push(ir::Field {
                name: field.name.text.clone(),
                ty,
            });
        }
    }
    Ok(())
}

fn invariants(source: &ast::Program, program: &mut ir::Program, globals: &Globals) -> Checked<()> {
    let mut names: BTreeMap<String, Position> = BTreeMap::new();
    for item in &source.items {
        let Item::Invariant(definition) = item else {
            continue;
        };
        if let Some(previous) = names.insert(definition.name.text.clone(), definition.name.at) {
            return error(
                definition.name.at,
                format!(
                    "invariant '{}' is already declared at {previous}",
                    definition.name.text
                ),
            );
        }
        let checked = handler::invariant(program, globals, definition)?;
        program.invariants.push(checked);
    }
    Ok(())
}

fn handlers(source: &ast::Program, program: &mut ir::Program, globals: &Globals) -> Checked<()> {
    let mut names: BTreeMap<String, Position> = BTreeMap::new();
    for item in &source.items {
        let Item::Handler(definition) = item else {
            continue;
        };
        if let Some(previous) = names.insert(definition.name.text.clone(), definition.name.at) {
            return error(
                definition.name.at,
                format!(
                    "handler '{}' is already declared at {previous}",
                    definition.name.text
                ),
            );
        }
        let checked = handler::check(program, globals, definition)?;
        program.handlers.push(checked);
    }
    if program.handlers.is_empty() {
        return error(
            source.name.at,
            format!("harness '{}' has no handler", source.name.text),
        );
    }
    Ok(())
}
