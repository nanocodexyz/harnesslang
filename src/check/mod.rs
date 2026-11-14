mod handler;
mod inline;

pub use inline::STATEMENTS_PER_HANDLER_MAX;

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{self, Bound, Sum, SumKind, Type, Variant};
use crate::syntax::ast::{self, Item, TypeBody, TypeExpr};
use crate::syntax::{Diagnostic, Position};

pub const BUILTIN_TYPES: [&str; 6] = ["u64", "bool", "text", "list", "known", "maybe"];
pub const BUILTIN_SUM_NAMES: [&str; 3] = ["call", "reply", "entry"];
pub const BUILTIN_FUNCTIONS: [&str; 11] = [
    "dispatch",
    "now",
    "random",
    "len",
    "wait",
    "suspend",
    "contains",
    "before",
    "after",
    "trim",
    "truncate_middle",
];
pub const WILDCARD: &str = "_";
pub const EVENT_CANCEL: &str = "cancel";
pub const EVENT_STEER: &str = "steer";
pub const EVENT_RESUME: &str = "resume";
pub const STEER_PARAMETER: &str = "message";
pub const CANCELLED_READ: &str = "cancelled";
pub const STEERS_READ: &str = "steers";

pub struct Globals {
    constants: BTreeMap<String, ir::ConstId>,
    types: BTreeMap<String, ir::SumId>,
    tools: BTreeMap<String, ir::ToolId>,
    codes: BTreeMap<String, ir::CodeId>,
    listings: BTreeMap<String, ir::ListingId>,
    fields: BTreeMap<String, ir::FieldId>,
    model: Option<String>,
}

impl Globals {
    fn effect(&self, name: &str) -> bool {
        self.tools.contains_key(name)
            || self.listings.contains_key(name)
            || self.codes.contains_key(name)
            || self.model.as_deref() == Some(name)
    }
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
        codes: Vec::new(),
        listings: Vec::new(),
        model: None,
        state: Vec::new(),
        invariants: Vec::new(),
        handlers: Vec::new(),
        events: ir::Events::default(),
        cuts: Vec::new(),
    };
    let mut globals = Globals {
        constants: BTreeMap::new(),
        types: BTreeMap::new(),
        tools: BTreeMap::new(),
        codes: BTreeMap::new(),
        listings: BTreeMap::new(),
        fields: BTreeMap::new(),
        model: None,
    };
    let mut values: BTreeMap<String, Position> = BTreeMap::new();
    constants(source, &mut program, &mut globals, &mut values)?;
    model(source, &mut program, &mut globals, &mut values)?;
    sums(source, &mut program, &mut globals)?;
    tools(source, &mut program, &mut globals, &mut values)?;
    codes(source, &mut program, &mut globals, &mut values)?;
    listings(source, &mut program, &mut globals, &mut values)?;
    state(source, &mut program, &mut globals, &mut values)?;
    events(source, &mut program, &globals, &mut values)?;
    let expanded = inline::expand(source, &program, &globals, &mut values)?;
    invariants(&expanded, &mut program, &globals)?;
    handlers(&expanded, &mut program, &globals)?;
    program.cuts = program.find_cuts();
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
        let mut modes: Vec<String> = Vec::with_capacity(model.modes.len());
        for mode in &model.modes {
            if modes.contains(&mode.text) {
                return error(
                    mode.at,
                    format!(
                        "model '{}' declares mode '{}' twice",
                        model.name.text, mode.text
                    ),
                );
            }
            modes.push(mode.text.clone());
        }
        globals.model = Some(model.name.text.clone());
        program.model = Some(ir::Model {
            name: model.name.text.clone(),
            calls,
            reply: ir::Reply::Builtin,
            window: None,
            modes,
        });
    }
    Ok(())
}

fn declared_reply(source: &ast::Program) -> Option<&ast::Name> {
    source.items.iter().find_map(|item| match item {
        Item::Model(model) => model.reply.as_ref(),
        _ => None,
    })
}

fn builtin_sums(program: &ir::Program, declared: Option<&str>) -> Vec<Sum> {
    let call = Sum {
        name: BUILTIN_SUM_NAMES[ir::CALL].to_owned(),
        kind: SumKind::Tagged,
        variants: vec![Variant {
            name: BUILTIN_SUM_NAMES[ir::CALL].to_owned(),
            fields: vec![Type::Text, Type::Text, Type::Text],
        }],
    };
    let Some(model) = &program.model else {
        let placeholder = |name: &str| Sum {
            name: name.to_owned(),
            kind: SumKind::Tagged,
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
    let reply = match declared {
        Some(name) => Sum {
            name: name.to_owned(),
            kind: SumKind::Tagged,
            variants: Vec::new(),
        },
        None => Sum {
            name: BUILTIN_SUM_NAMES[ir::REPLY].to_owned(),
            kind: SumKind::Tagged,
            variants: vec![
                variant("answer", vec![Type::Text]),
                variant("calls", vec![calls]),
            ],
        },
    };
    let call_type = Type::Sum(ir::CALL);
    let entry = Sum {
        name: BUILTIN_SUM_NAMES[ir::ENTRY].to_owned(),
        kind: SumKind::Tagged,
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

fn declared_window(source: &ast::Program) -> Option<&ast::Name> {
    source.items.iter().find_map(|item| match item {
        Item::Model(model) => model.window.as_ref(),
        _ => None,
    })
}

fn window(source: &ast::Program, program: &mut ir::Program, globals: &mut Globals) -> Checked<()> {
    let Some(name) = declared_window(source) else {
        return Ok(());
    };
    let tokens = bound(program, globals, name, "the model's context window")?;
    if tokens.value == 0 {
        return error(
            name.at,
            format!("the model's context window '{}' is 0 tokens", name.text),
        );
    }
    let stop = program.sums.len();
    program.sums.push(Sum {
        name: ir::STOP_TYPE.to_owned(),
        kind: SumKind::Text,
        variants: ir::STOP_VALUES
            .iter()
            .map(|value| Variant {
                name: (*value).to_owned(),
                fields: Vec::new(),
            })
            .collect(),
    });
    let attempt = program.sums.len();
    program.sums.push(Sum {
        name: ir::ATTEMPT_TYPE.to_owned(),
        kind: SumKind::Tagged,
        variants: ir::attempt_variants(stop),
    });
    globals.types.insert(ir::STOP_TYPE.to_owned(), stop);
    globals.types.insert(ir::ATTEMPT_TYPE.to_owned(), attempt);
    program
        .model
        .as_mut()
        .expect("a window belongs to the model")
        .window = Some(ir::Window {
        tokens,
        stop,
        attempt,
    });
    Ok(())
}

fn sums(source: &ast::Program, program: &mut ir::Program, globals: &mut Globals) -> Checked<()> {
    let reply = declared_reply(source);
    program.sums = builtin_sums(program, reply.map(|name| name.text.as_str()));
    for (id, name) in BUILTIN_SUM_NAMES.iter().enumerate() {
        if id != ir::REPLY || reply.is_none() {
            globals.types.insert((*name).to_owned(), id);
        }
    }
    window(source, program, globals)?;
    let mut declared = Vec::new();
    for item in &source.items {
        let Item::Type(definition) = item else {
            continue;
        };
        register(program, globals, definition, reply)?;
        declared.push(definition);
    }
    if let Some(reply) = reply
        && globals.types.get(&reply.text) != Some(&ir::REPLY)
    {
        return error(
            reply.at,
            format!(
                "the model's reply type '{}' is not a type this harness declares",
                reply.text
            ),
        );
    }
    for definition in &declared {
        let id = globals.types[&definition.name.text];
        program.sums[id].variants = match &definition.body {
            TypeBody::Variants(variants) => {
                tagged_variants(program, globals, definition, variants)?
            }
            TypeBody::Values(values) => text_values(definition, values)?,
        };
    }
    acyclic(source, program, globals)?;
    let Some(reply) = reply else {
        return Ok(());
    };
    let definition = declared
        .iter()
        .find(|definition| definition.name.text == reply.text)
        .expect("the reply type is declared, checked above");
    let shape = reply_shape(program, definition)?;
    program
        .model
        .as_mut()
        .expect("a declared reply belongs to the model")
        .reply = shape;
    Ok(())
}

fn register(
    program: &mut ir::Program,
    globals: &mut Globals,
    definition: &ast::TypeDef,
    reply: Option<&ast::Name>,
) -> Checked<()> {
    let name = &definition.name;
    if BUILTIN_TYPES.contains(&name.text.as_str()) || globals.types.contains_key(&name.text) {
        return error(
            name.at,
            format!("type '{}' is already declared or builtin", name.text),
        );
    }
    let kind = match definition.body {
        TypeBody::Variants(_) => SumKind::Tagged,
        TypeBody::Values(_) => SumKind::Text,
    };
    if !reply.is_some_and(|reply| reply.text == name.text) {
        globals.types.insert(name.text.clone(), program.sums.len());
        program.sums.push(Sum {
            name: name.text.clone(),
            kind,
            variants: Vec::new(),
        });
        return Ok(());
    }
    if kind == SumKind::Text {
        return error(
            name.at,
            format!(
                "reply type '{}' is a text enum: a reply is a sum whose variants carry fields",
                name.text
            ),
        );
    }
    globals.types.insert(name.text.clone(), ir::REPLY);
    Ok(())
}

fn reply_shape(program: &ir::Program, definition: &ast::TypeDef) -> Checked<ir::Reply> {
    let model = program
        .model
        .as_ref()
        .expect("a declared reply belongs to the model");
    let TypeBody::Variants(declared) = &definition.body else {
        unreachable!("a text enum reply is refused when it is declared");
    };
    let reply = &definition.name.text;
    let calls = Type::List {
        element: Box::new(Type::Sum(ir::CALL)),
        capacity: model.calls.clone(),
    };
    let sum = &program.sums[ir::REPLY];
    let mut carrier: Option<(usize, usize)> = None;
    for (index, (variant, source)) in sum.variants.iter().zip(declared).enumerate() {
        if variant.name == ir::MODEL_FAILED {
            return error(
                source.name.at,
                format!(
                    "reply type '{reply}' has a variant named '{}', the name a failed model call is recorded under",
                    ir::MODEL_FAILED
                ),
            );
        }
        for (field_index, (field, at)) in variant.fields.iter().zip(&source.fields).enumerate() {
            if *field == calls {
                if let Some((previous, _)) = carrier {
                    return error(
                        at.at(),
                        format!(
                            "reply type '{reply}' carries {} in '{}' and '{}': exactly one variant carries the model's calls",
                            field.name(program),
                            sum.variants[previous].name,
                            variant.name
                        ),
                    );
                }
                carrier = Some((index, field_index));
                continue;
            }
            reply_field(program, reply, &variant.name, field, &calls, at.at())?;
        }
    }
    let Some((calls_variant, field)) = carrier else {
        return error(
            definition.name.at,
            format!(
                "reply type '{reply}' has no variant carrying {}: exactly one variant carries the model's calls",
                calls.name(program)
            ),
        );
    };
    Ok(ir::Reply::Declared {
        calls: calls_variant,
        field,
    })
}

fn reply_field(
    program: &ir::Program,
    reply: &str,
    variant: &str,
    field: &Type,
    calls: &Type,
    at: Position,
) -> Checked<()> {
    let scalar = matches!(field, Type::U64 | Type::Bool | Type::Text);
    if scalar || program.text_enum(field).is_some() {
        return Ok(());
    }
    let what = match field {
        Type::List { element, .. } if **element == Type::Sum(ir::CALL) => format!(
            "carries {}, but the model's calls bound is {}",
            field.name(program),
            calls.name(program)
        ),
        _ => format!(
            "carries {}: a reply's fields are u64, bool, text, text enums and the one {}",
            field.name(program),
            calls.name(program)
        ),
    };
    error(
        at,
        format!("variant '{variant}' of reply type '{reply}' {what}"),
    )
}

pub fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn tagged_variants(
    program: &ir::Program,
    globals: &Globals,
    definition: &ast::TypeDef,
    declared: &[ast::VariantDef],
) -> Checked<Vec<Variant>> {
    let mut seen = BTreeSet::new();
    let mut variants = Vec::new();
    for variant in declared {
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
    Ok(variants)
}

fn text_values(definition: &ast::TypeDef, values: &[ast::Name]) -> Checked<Vec<Variant>> {
    let mut seen = BTreeSet::new();
    let mut variants = Vec::new();
    for value in values {
        if !seen.insert(value.text.clone()) {
            return error(
                value.at,
                format!(
                    "value {} appears twice in '{}'",
                    quoted(&value.text),
                    definition.name.text
                ),
            );
        }
        variants.push(Variant {
            name: value.text.clone(),
            fields: Vec::new(),
        });
    }
    Ok(variants)
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
                None if other == BUILTIN_SUM_NAMES[ir::REPLY]
                    && program.model.as_ref().is_some_and(ir::Model::declared) =>
                {
                    error(
                        name.at,
                        format!(
                            "unknown type 'reply': the model replies with '{}'",
                            program.sums[ir::REPLY].name
                        ),
                    )
                }
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
    for sum in program
        .sums
        .iter()
        .filter(|sum| sum.kind == SumKind::Tagged)
    {
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
        let params = params(program, globals, &tool.params)?;
        let defaults = defaults(program, globals, &params, &tool.defaults)?;
        let returns = resolve(program, globals, &tool.returns)?;
        if !matches!(returns, Type::U64 | Type::Bool | Type::Text)
            && program.text_enum(&returns).is_none()
        {
            return error(
                tool.returns.at(),
                "a tool returns u64, bool, text or a text enum",
            );
        }
        globals
            .tools
            .insert(tool.name.text.clone(), program.tools.len());
        program.tools.push(ir::Tool {
            name: tool.name.text.clone(),
            params,
            defaults,
            returns,
            effect: tool.effect,
        });
    }
    Ok(())
}

fn params(
    program: &ir::Program,
    globals: &Globals,
    declared: &[ast::Param],
) -> Checked<Vec<ir::Field>> {
    let mut params = Vec::new();
    let mut seen = BTreeSet::new();
    for param in declared {
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
    Ok(params)
}

fn defaults(
    program: &ir::Program,
    globals: &Globals,
    params: &[ir::Field],
    declared: &[Option<ast::Expr>],
) -> Checked<Vec<Option<ir::Literal>>> {
    assert_eq!(
        params.len(),
        declared.len(),
        "the parser gives every tool parameter a default slot"
    );
    params
        .iter()
        .zip(declared)
        .map(|(param, default)| {
            default
                .as_ref()
                .map(|default| default_value(program, globals, param, default))
                .transpose()
        })
        .collect()
}

fn default_value(
    program: &ir::Program,
    globals: &Globals,
    param: &ir::Field,
    default: &ast::Expr,
) -> Checked<ir::Literal> {
    let literal = match (&param.ty, default) {
        (Type::U64, ast::Expr::Number(value, _)) => Some(ir::Literal::U64(*value)),
        (Type::U64, ast::Expr::Name(name)) => globals
            .constants
            .get(&name.text)
            .map(|id| ir::Literal::U64(program.constants[*id].value)),
        (Type::Bool, ast::Expr::Bool(value, _)) => Some(ir::Literal::Bool(*value)),
        (Type::Text, ast::Expr::Text(value, _)) => Some(ir::Literal::Text(value.clone())),
        (ty, ast::Expr::Text(value, _)) => program
            .text_enum(ty)
            .filter(|sum| sum.variants.iter().any(|variant| variant.name == *value))
            .map(|_| ir::Literal::Text(value.clone())),
        _ => None,
    };
    literal.ok_or_else(|| {
        Diagnostic::new(
            default.at(),
            format!(
                "the default of parameter '{}' must be a literal of {} or, for u64, a named constant",
                param.name,
                param.ty.name(program)
            ),
        )
    })
}

fn listings(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    let variants = variant_names(program);
    for item in &source.items {
        let Item::Listing(listing) = item else {
            continue;
        };
        let name = &listing.name;
        claim(values, name, "listing")?;
        if variants.contains(&name.text) {
            return error(
                name.at,
                format!("listing '{}' has the name of a variant", name.text),
            );
        }
        if name.text.contains(ir::LISTED_TOOL_SEPARATOR) || name.text.ends_with(WILDCARD) {
            return error(
                name.at,
                format!(
                    "listing '{}' names its tools '{}{}NAME', so its name holds no '{}' and does not end with '_'",
                    name.text,
                    name.text,
                    ir::LISTED_TOOL_SEPARATOR,
                    ir::LISTED_TOOL_SEPARATOR
                ),
            );
        }
        let bound = bound(program, globals, &listing.bound, "a listing's bound")?;
        let params = params(program, globals, &listing.params)?;
        let defaults = defaults(program, globals, &params, &listing.defaults)?;
        globals
            .listings
            .insert(name.text.clone(), program.listings.len());
        program.listings.push(ir::Listing {
            name: name.text.clone(),
            bound,
            params,
            defaults,
            effect: listing.effect,
        });
    }
    for item in &source.items {
        let Item::Tool(tool) = item else { continue };
        if let Some((id, _)) = program.listed(&tool.name.text) {
            return error(
                tool.name.at,
                format!(
                    "tool '{}' is named like a tool of listing '{}'",
                    tool.name.text, program.listings[id].name
                ),
            );
        }
    }
    Ok(())
}

pub const CODE_FIELDS: [&str; 6] = [
    "calls",
    "parallel",
    "timers",
    "timer_ms",
    "journal_bytes",
    "yield_ms",
];

fn codes(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    let variants = variant_names(program);
    let declared: Vec<&ast::Code> = source
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Code(code) => Some(code),
            _ => None,
        })
        .collect();
    for (id, code) in declared.iter().enumerate() {
        claim(values, &code.name, "code tool")?;
        if variants.contains(&code.name.text) {
            return error(
                code.name.at,
                format!("code tool '{}' has the name of a variant", code.name.text),
            );
        }
        globals.codes.insert(code.name.text.clone(), id);
    }
    for code in declared {
        let checked = code_tool(program, globals, code)?;
        program.codes.push(checked);
    }
    Ok(())
}

fn code_tool(program: &ir::Program, globals: &Globals, code: &ast::Code) -> Checked<ir::Code> {
    let name = &code.name.text;
    if program.model.is_none() {
        return error(
            code.name.at,
            format!("code '{name}' needs a model declaration: its cells are written by the model"),
        );
    }
    let tools = nested_tools(globals, code)?;
    let mut limits: BTreeMap<&str, Bound> = BTreeMap::new();
    for field in &code.fields {
        let Some(key) = CODE_FIELDS.iter().find(|key| **key == field.key.text) else {
            return error(
                field.key.at,
                format!(
                    "code '{name}' has no field '{}'; its fields are tools, {}",
                    field.key.text,
                    CODE_FIELDS.join(", ")
                ),
            );
        };
        let what = format!("field '{key}' of code '{name}'");
        if limits
            .insert(key, bound(program, globals, &field.value, &what)?)
            .is_some()
        {
            return error(field.key.at, format!("code '{name}' gives '{key}' twice"));
        }
    }
    let mut take = |key: &str| match limits.remove(key) {
        Some(limit) => Ok(limit),
        None => error(code.name.at, format!("code '{name}' lacks '{key}'")),
    };
    let checked = ir::Code {
        name: name.clone(),
        tools,
        calls: take(CODE_FIELDS[0])?,
        parallel: take(CODE_FIELDS[1])?,
        timers: take(CODE_FIELDS[2])?,
        timer_ms: take(CODE_FIELDS[3])?,
        journal_bytes: take(CODE_FIELDS[4])?,
        yield_ms: take(CODE_FIELDS[5])?,
    };
    code_bounds(code, &checked)?;
    Ok(checked)
}

fn nested_tools(globals: &Globals, code: &ast::Code) -> Checked<Vec<ir::ToolId>> {
    let name = &code.name.text;
    if code.tools.is_empty() {
        return error(
            code.name.at,
            format!("code '{name}' lists no tools: a cell's every effect is a nested call"),
        );
    }
    let mut tools = Vec::new();
    for tool in &code.tools {
        if globals.codes.contains_key(&tool.text) {
            return error(
                tool.at,
                format!(
                    "code '{name}' lists code tool '{}': cells call declared tools, not cells",
                    tool.text
                ),
            );
        }
        let Some(&id) = globals.tools.get(&tool.text) else {
            return error(
                tool.at,
                format!(
                    "code '{name}' lists '{}', which is not a declared tool",
                    tool.text
                ),
            );
        };
        if tools.contains(&id) {
            return error(
                tool.at,
                format!("code '{name}' lists tool '{}' twice", tool.text),
            );
        }
        tools.push(id);
    }
    Ok(tools)
}

fn code_bounds(code: &ast::Code, checked: &ir::Code) -> Checked<()> {
    let name = &code.name.text;
    if checked.parallel.value > checked.calls.value {
        return error(
            code.name.at,
            format!(
                "code '{name}': parallel ({} = {}) exceeds calls ({} = {})",
                checked.parallel.name,
                checked.parallel.value,
                checked.calls.name,
                checked.calls.value
            ),
        );
    }
    if checked.parallel.value == 0 {
        return error(
            code.name.at,
            format!(
                "code '{name}': parallel ({} = 0) lets no nested call start",
                checked.parallel.name
            ),
        );
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
            if crate::run::value::Value::zero(program, &ty).is_none() {
                return error(
                    field.ty.at(),
                    format!(
                        "state field '{}' has no initial value: state fields are u64, bool, text, text enums or lists",
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

fn events(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    for item in &source.items {
        let Item::Event(event) = item else { continue };
        match event.name.text.as_str() {
            EVENT_CANCEL => cancel_event(event, program, values)?,
            EVENT_STEER => steer_event(event, program, globals)?,
            EVENT_RESUME => resume_event(event, program)?,
            other => {
                return error(
                    event.name.at,
                    format!(
                        "unknown event '{other}': a harness takes '{EVENT_CANCEL}', '{EVENT_STEER}' and '{EVENT_RESUME}'"
                    ),
                );
            }
        }
    }
    if program.events.cancel && variant_names(program).contains(CANCELLED_READ) {
        let at = source
            .items
            .iter()
            .find_map(|item| match item {
                Item::Type(definition) => match &definition.body {
                    TypeBody::Variants(variants) => variants
                        .iter()
                        .find(|variant| variant.name.text == CANCELLED_READ)
                        .map(|variant| variant.name.at),
                    TypeBody::Values(_) => None,
                },
                _ => None,
            })
            .expect("a variant name comes from a type definition");
        return error(
            at,
            format!("variant '{CANCELLED_READ}' hides the cancel event's read: rename it"),
        );
    }
    Ok(())
}

fn cancel_event(
    event: &ast::Event,
    program: &mut ir::Program,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    if !event.params.is_empty() || event.bound.is_some() {
        return error(
            event.name.at,
            format!("event '{EVENT_CANCEL}' carries nothing: write 'on {EVENT_CANCEL}'"),
        );
    }
    if program.events.cancel {
        return error(
            event.name.at,
            format!("event '{EVENT_CANCEL}' is already declared"),
        );
    }
    let read = ast::Name {
        text: CANCELLED_READ.to_owned(),
        at: event.name.at,
    };
    claim(values, &read, "the cancel event's read")?;
    program.events.cancel = true;
    Ok(())
}

fn resume_event(event: &ast::Event, program: &mut ir::Program) -> Checked<()> {
    if !event.params.is_empty() || event.bound.is_some() {
        return error(
            event.name.at,
            format!(
                "event '{EVENT_RESUME}' carries the key of a suspend and a text answer: write 'on {EVENT_RESUME}'"
            ),
        );
    }
    if program.events.resume {
        return error(
            event.name.at,
            format!("event '{EVENT_RESUME}' is already declared"),
        );
    }
    program.events.resume = true;
    Ok(())
}

fn steer_event(event: &ast::Event, program: &mut ir::Program, globals: &Globals) -> Checked<()> {
    let shape = format!("on {EVENT_STEER}({STEER_PARAMETER}: text) bound N");
    let one_text = match event.params.as_slice() {
        [param] => {
            param.name.text == STEER_PARAMETER
                && matches!(&param.ty, TypeExpr::Named(name) if name.text == "text")
        }
        _ => false,
    };
    let Some(limit) = event.bound.as_ref().filter(|_| one_text) else {
        return error(
            event.name.at,
            format!("event '{EVENT_STEER}' carries one text: write '{shape}'"),
        );
    };
    if program.events.steers.is_some() {
        return error(
            event.name.at,
            format!("event '{EVENT_STEER}' is already declared"),
        );
    }
    let bound = bound(program, globals, limit, "the steers one turn records")?;
    program.events.steers = Some(bound);
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
